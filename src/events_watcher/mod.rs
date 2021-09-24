use std::cmp;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use futures::prelude::*;
use webb::evm::ethers::providers::Middleware;
use webb::evm::ethers::{contract, providers, types};

use crate::store::HistoryStore;

mod anchor_leaves_watcher;
pub use anchor_leaves_watcher::*;

mod anchor2_watcher;
pub use anchor2_watcher::*;

mod bridge_watcher;
pub use bridge_watcher::*;

/// A watchable contract is a contract used in the [EventWatcher]
pub trait WatchableContract: Send + Sync {
    /// The block number where this contract is deployed.
    fn deployed_at(&self) -> types::U64;

    /// How often this contract should be polled for events.
    fn polling_interval(&self) -> Duration;
}

#[async_trait::async_trait]
pub trait EventWatcher {
    type Middleware: providers::Middleware + 'static;
    type Contract: Deref<Target = contract::Contract<Self::Middleware>>
        + WatchableContract;
    type Events: contract::EthLogDecode;
    type Store: HistoryStore;

    async fn handle_event(
        &self,
        store: Arc<Self::Store>,
        contract: &Self::Contract,
        (event, log): (Self::Events, contract::LogMeta),
    ) -> anyhow::Result<()>;

    /// Returns a task that should be running in the background
    /// that will watch events
    #[tracing::instrument(
        skip(self, client, store, contract),
        fields(contract = %contract.address())
    )]
    async fn run(
        &self,
        client: Arc<Self::Middleware>,
        store: Arc<Self::Store>,
        contract: Self::Contract,
    ) -> anyhow::Result<()> {
        let backoff = backoff::ExponentialBackoff {
            max_elapsed_time: None,
            ..Default::default()
        };
        let task = || async {
            let step = types::U64::from(50);
            // now we start polling for new events.
            loop {
                let block = store.get_last_block_number(
                    contract.address(),
                    contract.deployed_at(),
                )?;
                let current_block_number = client
                    .get_block_number()
                    .map_err(anyhow::Error::from)
                    .await?;
                tracing::trace!(
                    "Latest block number: #{}",
                    current_block_number
                );
                let dest_block = cmp::min(block + step, current_block_number);
                // check if we are now on the latest block.
                let should_cooldown = dest_block == current_block_number;
                tracing::trace!("Reading from #{} to #{}", block, dest_block);
                let events_filter = contract
                    .event_with_filter::<Self::Events>(Default::default())
                    .from_block(block)
                    .to_block(dest_block);
                let found_events = events_filter
                    .query_with_meta()
                    .map_err(anyhow::Error::from)
                    .await?;

                tracing::trace!("Found #{} events", found_events.len());

                for (event, log) in found_events {
                    let result = self
                        .handle_event(
                            store.clone(),
                            &contract,
                            (event, log.clone()),
                        )
                        .await;
                    match result {
                        Ok(_) => {
                            store.set_last_block_number(
                                contract.address(),
                                log.block_number,
                            )?;
                            tracing::trace!(
                                "event handled successfully. at #{}",
                                log.block_number
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "Error while handling event: {}",
                                e
                            );
                            // this a transient error, so we will retry again.
                            return Err(backoff::Error::Transient(e));
                        }
                    }
                }
                // move forward.
                store.set_last_block_number(contract.address(), dest_block)?;
                tracing::trace!("Polled from #{} to #{}", block, dest_block);
                if should_cooldown {
                    let duration = contract.polling_interval();
                    tracing::trace!(
                        "Cooldown a bit for {}ms",
                        duration.as_millis()
                    );
                    tokio::time::sleep(duration).await;
                }
            }
        };
        backoff::future::retry(backoff, task).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait BridgeWatcher: EventWatcher {
    async fn handle_cmd(
        &self,
        store: Arc<Self::Store>,
        cmd: BridgeCommand,
    ) -> anyhow::Result<()>;

    /// Returns a task that should be running in the background
    /// that will watch for all commands
    #[tracing::instrument(
        skip(self, client, store, contract),
        fields(contract = %contract.address())
    )]
    async fn run(
        &self,
        client: Arc<Self::Middleware>,
        store: Arc<Self::Store>,
        contract: Self::Contract,
    ) -> anyhow::Result<()> {
        let backoff = backoff::ExponentialBackoff {
            max_elapsed_time: None,
            ..Default::default()
        };
        let task = || async {
            let my_address = contract.address();
            let my_chain_id =
                client.get_chainid().map_err(anyhow::Error::from).await?;
            let my_key = BridgeKey::new(my_address, my_chain_id);
            let rx = BridgeRegistry::register(my_key);
            let mut rx_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
            while let Some(cmd) = rx_stream.next().await {
                let result = self.handle_cmd(store.clone(), cmd).await;
                match result {
                    Ok(_) => {
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                        // this a transient error, so we will retry again.
                        return Err(backoff::Error::Transient(e));
                    }
                }
            }
            Ok(())
        };
        backoff::future::retry(backoff, task).await?;
        Ok(())
    }
}