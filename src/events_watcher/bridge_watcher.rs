use std::collections::HashMap;
use std::fmt::LowerHex;
use std::ops;
use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use webb::evm::contract::darkwebb::{BridgeContract, BridgeContractEvents};
use webb::evm::ethers::prelude::*;
use webb::evm::ethers::providers;
use webb::evm::ethers::types;
use webb::evm::ethers::utils;

use crate::config;
use crate::store::sled::SledStore;

use super::{BridgeWatcher, EventWatcher, ProposalStore, TxQueueStore};

type BridgeConnectionSender = tokio::sync::mpsc::Sender<BridgeCommand>;
type BridgeConnectionReceiver = tokio::sync::mpsc::Receiver<BridgeCommand>;
type Registry = RwLock<HashMap<BridgeKey, BridgeConnectionSender>>;
type HttpProvider = providers::Provider<providers::Http>;

static BRIDGE_REGISTRY: OnceCell<Registry> = OnceCell::new();

/// A BridgeKey is used as a key in the registry.
/// based on the bridge address and the chain id.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BridgeKey {
    address: types::Address,
    chain_id: types::U256,
}

impl BridgeKey {
    pub fn new(address: types::Address, chain_id: types::U256) -> Self {
        Self { address, chain_id }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProposalStatus {
    Inactive = 0,
    Active = 1,
    Passed = 2,
    Executed = 3,
    Cancelled = 4,
    Unknown = u8::MAX,
}

impl From<u8> for ProposalStatus {
    fn from(v: u8) -> Self {
        match v {
            0 => ProposalStatus::Inactive,
            1 => ProposalStatus::Active,
            2 => ProposalStatus::Passed,
            3 => ProposalStatus::Executed,
            4 => ProposalStatus::Cancelled,
            _ => ProposalStatus::Unknown,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProposalData {
    pub anchor_address: types::Address,
    pub anchor_handler_address: types::Address,
    pub origin_chain_id: types::U256,
    pub leaf_index: u32,
    pub merkle_root: [u8; 32],
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProposalEntity {
    pub origin_chain_id: types::U256,
    pub nonce: types::U64,
    pub data: Vec<u8>,
    pub data_hash: [u8; 32],
    pub resource_id: [u8; 32],
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BridgeCommand {
    CreateProposal(ProposalData),
}

/// A Bridge Registry is a simple Key-Value store, that provides an easy way to register
/// and discover bridges. For easy communication between the Anchors that connect to that bridge.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BridgeRegistry;

impl BridgeRegistry {
    /// Registers a new Bridge to the registry.
    /// This returns a BridgeConnectionReceiver which will receive commands from whoever
    /// would lookup for this bridge.
    pub fn register(key: BridgeKey) -> BridgeConnectionReceiver {
        let (tx, rx) = tokio::sync::mpsc::channel(50);
        let registry = BRIDGE_REGISTRY.get_or_init(Self::init_registry);
        registry.write().insert(key, tx);
        rx
    }

    /// Unregisters a Bridge from the registry.
    /// this will remove the bridge connection receiver from the registry and close any channels.
    #[allow(dead_code)]
    pub fn unregister(key: BridgeKey) {
        let registry = BRIDGE_REGISTRY.get_or_init(Self::init_registry);
        registry.write().remove(&key);
    }

    /// Lookup a bridge by key.
    /// Returns the BridgeConnectionSender which can be used to send commands to the bridge.
    pub fn lookup(key: BridgeKey) -> Option<BridgeConnectionSender> {
        let registry = BRIDGE_REGISTRY.get_or_init(Self::init_registry);
        registry.read().get(&key).cloned()
    }

    fn init_registry() -> Registry {
        RwLock::new(Default::default())
    }
}

#[derive(Clone, Debug)]
pub struct BridgeContractWrapper<M: Middleware> {
    config: config::BridgeContractConfig,
    contract: BridgeContract<M>,
}

impl<M: Middleware> BridgeContractWrapper<M> {
    pub fn new(config: config::BridgeContractConfig, client: Arc<M>) -> Self {
        Self {
            contract: BridgeContract::new(config.common.address, client),
            config,
        }
    }
}

impl<M: Middleware> ops::Deref for BridgeContractWrapper<M> {
    type Target = Contract<M>;

    fn deref(&self) -> &Self::Target {
        &self.contract
    }
}

impl<M: Middleware> super::WatchableContract for BridgeContractWrapper<M> {
    fn deployed_at(&self) -> types::U64 {
        self.config.common.deployed_at.into()
    }

    fn polling_interval(&self) -> Duration {
        Duration::from_millis(self.config.events_watcher.polling_interval)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BridgeContractWatcher;

#[async_trait::async_trait]
impl EventWatcher for BridgeContractWatcher {
    const TAG: &'static str = "Bridge Watcher";

    type Middleware = HttpProvider;

    type Contract = BridgeContractWrapper<Self::Middleware>;

    type Events = BridgeContractEvents;

    type Store = SledStore;

    #[tracing::instrument(
        skip_all,
        fields(ty = %to_event_type(&e.0)),
    )]
    async fn handle_event(
        &self,
        store: Arc<Self::Store>,
        wrapper: &Self::Contract,
        e: (Self::Events, LogMeta),
    ) -> anyhow::Result<()> {
        match e.0 {
            // check for every proposal
            // 1. if "executed" or "cancelled" -> remove it from the tx queue (if exists).
            // 2. if "passed" -> create a tx to execute the proposal.
            // 3. if "active" -> crate a tx to vote for it.
            BridgeContractEvents::ProposalEventFilter(e) => {
                match ProposalStatus::from(e.status) {
                    ProposalStatus::Executed | ProposalStatus::Cancelled => {
                        self.remove_proposal(
                            store,
                            &wrapper.contract,
                            &e.data_hash,
                        )
                        .await?;
                    }
                    ProposalStatus::Passed => {
                        self.execute_proposal(
                            store,
                            &wrapper.contract,
                            &e.data_hash,
                        )
                        .await?;
                    }
                    _ => {
                        // shall we watch also for active proposal?
                        // like should we vote when we see an active proposal
                        // that we already have not seen before? or we should
                        // just wait until we see it's event on the other chain?
                    }
                }
            }
            _ => {
                tracing::trace!("Got Event {:?}", e.0);
            }
        };
        Ok(())
    }
}

#[async_trait::async_trait]
impl BridgeWatcher for BridgeContractWatcher {
    #[tracing::instrument(skip_all)]
    async fn handle_cmd(
        &self,
        store: Arc<Self::Store>,
        wrapper: &Self::Contract,
        cmd: BridgeCommand,
    ) -> anyhow::Result<()> {
        use BridgeCommand::*;
        tracing::trace!("Got cmd {:?}", cmd);
        match cmd {
            CreateProposal(data) => {
                self.create_proposal(store, &wrapper.contract, data).await?;
            }
        };
        Ok(())
    }
}

impl BridgeContractWatcher
where
    Self: BridgeWatcher,
{
    #[tracing::instrument(skip_all)]
    async fn create_proposal(
        &self,
        store: Arc<<Self as EventWatcher>::Store>,
        contract: &BridgeContract<<Self as EventWatcher>::Middleware>,
        data: ProposalData,
    ) -> anyhow::Result<()> {
        let dest_chain_id = contract.client().get_chainid().await?;
        let update_data = create_update_proposal_data(
            data.origin_chain_id,
            data.leaf_index,
            data.merkle_root,
        );
        let data_bytes = hex::decode(&update_data)?;
        let pre_hashed =
            format!("{:x}{}", data.anchor_handler_address, update_data);
        let data_to_be_hashed = hex::decode(pre_hashed)?;
        let data_hash = utils::keccak256(data_to_be_hashed);
        let resource_id =
            create_resource_id(data.anchor_address, dest_chain_id)?;
        let entity = ProposalEntity {
            origin_chain_id: data.origin_chain_id,
            data: data_bytes,
            data_hash,
            nonce: types::U64::from(data.leaf_index),
            resource_id,
        };
        let contract_handler_address = contract
            .resource_id_to_handler_address(resource_id)
            .call()
            .await?;
        // sanity check
        assert_eq!(contract_handler_address, data.anchor_handler_address);
        let (status, ..) = contract
            .get_proposal(data.origin_chain_id, data.leaf_index as _, data_hash)
            .call()
            .await?;
        let status = ProposalStatus::from(status);
        if status >= ProposalStatus::Passed {
            tracing::debug!("Skipping this proposal ... already {:?}", status);
            return Ok(());
        }
        let call = contract.vote_proposal(
            entity.origin_chain_id,
            entity.nonce.as_u64(),
            entity.resource_id,
            entity.data_hash,
        );
        tracing::debug!(
            "Voting for Proposal 0x{} with resourceID 0x{}",
            hex::encode(&data_hash),
            hex::encode(&entity.resource_id),
        );
        // enqueue the transaction.
        store.enqueue_tx_with_key(&data_hash, call.tx, dest_chain_id)?;
        // save the proposal for later updates.
        store.insert_proposal(entity)?;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn remove_proposal(
        &self,
        store: Arc<<Self as EventWatcher>::Store>,
        contract: &BridgeContract<<Self as EventWatcher>::Middleware>,
        data_hash: &[u8],
    ) -> anyhow::Result<()> {
        let chain_id = contract.client().get_chainid().await?;
        store.remove_proposal(data_hash)?;
        // it is okay, if the proposal tx is not stored in
        // the queue, so it is okay to ignore the error in this case.
        let _ = store.remove_tx(data_hash, chain_id);
        tracing::debug!("Removed proposal 0x{}", hex::encode(&data_hash));
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn execute_proposal(
        &self,
        store: Arc<<Self as EventWatcher>::Store>,
        contract: &BridgeContract<<Self as EventWatcher>::Middleware>,
        data_hash: &[u8],
    ) -> anyhow::Result<()> {
        let chain_id = contract.client().get_chainid().await?;
        let entity = match store.remove_proposal(data_hash)? {
            Some(v) => v,
            None => {
                tracing::warn!(
                    "no proposal with 0x{} found locally (skipping)",
                    hex::encode(&data_hash)
                );
                return Ok(());
            }
        };
        // before trying to execute the proposal, we need to
        // double check that the proposal is not already executed.
        //
        // why we do the check?
        // since sometimes the relayer would be offline for a bit, and then it sees
        // that this proposal is passed (from the events as it sync) but in the current
        // time, this proposal is already executed (since this event is from the past).
        // that's why we need to do this check here.
        let (status, ..) = contract
            .get_proposal(
                entity.origin_chain_id,
                entity.nonce.as_u64(),
                entity.data_hash,
            )
            .call()
            .await?;
        let status = ProposalStatus::from(status);
        if status >= ProposalStatus::Executed {
            tracing::debug!(
                "Skipping execution of proposal 0x{} since it is already {:?}",
                hex::encode(data_hash),
                status
            );
            return Ok(());
        }
        // and also assert it is passed.
        assert_eq!(status, ProposalStatus::Passed);
        let call = contract.execute_proposal(
            entity.origin_chain_id,
            entity.nonce.as_u64(),
            entity.data,
            entity.resource_id,
        );
        tracing::debug!(
            "Executing proposal 0x{} with resourceID 0x{}",
            hex::encode(data_hash),
            hex::encode(&entity.resource_id),
        );
        // enqueue the transaction.
        store.enqueue_tx_with_key(data_hash, call.tx, chain_id)?;
        Ok(())
    }
}

fn create_update_proposal_data(
    chain_id: types::U256,
    leaf_index: u32,
    merkle_root: [u8; 32],
) -> String {
    let chain_id_hex = to_hex(chain_id, 32);
    let leaf_index_hex = to_hex(leaf_index, 32);
    let merkle_root_hex = hex::encode(&merkle_root);
    format!("{}{}{}", chain_id_hex, leaf_index_hex, merkle_root_hex)
}

fn create_resource_id(
    anchor_address: types::Address,
    chain_id: types::U256,
) -> anyhow::Result<[u8; 32]> {
    let truncated = to_hex(chain_id, 4);
    let result = format!("{:x}{}", anchor_address, truncated);
    let hash = hex::decode(result)?;
    let mut result_bytes = [0u8; 32];
    result_bytes
        .iter_mut()
        .skip(32 - hash.len())
        .zip(hash)
        .for_each(|(r, h)| *r = h);
    Ok(result_bytes)
}

fn to_hex(value: impl LowerHex, padding: usize) -> String {
    let mut hexed = format!("{:x}", value);
    if hexed.len() % 2 != 0 {
        hexed = String::from("0") + &hexed;
    }
    while hexed.len() < 2 * padding {
        hexed = String::from("0") + &hexed;
    }
    hexed
}

fn to_event_type(event: &BridgeContractEvents) -> &str {
    match event {
        BridgeContractEvents::PausedFilter(_) => "Paused",
        BridgeContractEvents::ProposalEventFilter(_) => "ProposalEvent",
        BridgeContractEvents::ProposalVoteFilter(_) => "ProposalVote",
        BridgeContractEvents::RelayerAddedFilter(_) => "RelayerAdded",
        BridgeContractEvents::RelayerRemovedFilter(_) => "RelayerRemoved",
        BridgeContractEvents::RelayerThresholdChangedFilter(_) => {
            "RelayerThresholdChanged"
        }
        BridgeContractEvents::RoleGrantedFilter(_) => "RoleGranted",
        BridgeContractEvents::RoleRevokedFilter(_) => "RoleRevoked",
        BridgeContractEvents::UnpausedFilter(_) => "Unpaused",
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn should_create_update_proposal() {
        let chain_id = types::U256::from(4);
        let leaf_index = 1u32;
        let merkle_root = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
            19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
        ];
        let result =
            create_update_proposal_data(chain_id, leaf_index, merkle_root);
        let expected = include_str!("../../tests/fixtures/proposal_data.txt")
            .trim_end_matches('\n');
        assert_eq!(result, expected);
        let dest_handler = types::Address::from_str(
            "0x7Bb1Af8D06495E85DDC1e0c49111C9E0Ab50266E",
        )
        .unwrap();
        let pre_hashed = format!("{:x}{}", dest_handler, result);
        let data_to_be_hashed = hex::decode(pre_hashed).unwrap();
        let data_hash = hex::encode(utils::keccak256(data_to_be_hashed));
        let expected_data_hash =
            "45822e043e5735fc2485e52dd71403d140f3a755cd59dc02539eaef3bcfd4bcb";
        assert_eq!(data_hash, expected_data_hash);
    }

    #[test]
    fn should_create_resouce_id() {
        let chain_id = types::U256::from(4);
        let anchor_address = types::Address::from_str(
            "0xB42139fFcEF02dC85db12aC9416a19A12381167D",
        )
        .unwrap();
        let resource_id =
            create_resource_id(anchor_address, chain_id).unwrap();
        let expected = hex::decode(
            "0000000000000000b42139ffcef02dc85db12ac9416a19a12381167d00000004",
        )
        .unwrap();
        assert_eq!(resource_id, expected.as_slice());
    }
}
