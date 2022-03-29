// This our basic EVM Transaction Relayer Tests.
// These are for testing the basic relayer functionality. which is just relay transactions for us.

import { expect } from 'chai';
import { Bridges, Tokens } from '@webb-tools/protocol-solidity';
import { ethers } from 'ethers';
import temp from 'temp';
import { LocalChain } from '../lib/localTestnet';
import { calcualteRelayerFees, WebbRelayer } from '../lib/webbRelayer';

describe.skip('EVM Transaction Relayer', function () {
  this.timeout(120_000);
  const PK1 =
    '0xc0d375903fd6f6ad3edafc2c5428900c0757ce1da10e5dd864fe387b32b91d7e';
  const PK2 =
    '0xc0d375903fd6f6ad3edafc2c5428900c0757ce1da10e5dd864fe387b32b91d7f';
  const tmp = temp.track();

  const tmpDirPath = tmp.mkdirSync({ prefix: 'webb-relayer-test-' });
  let localChain1: LocalChain;
  let localChain2: LocalChain;
  let signatureBridge: Bridges.SignatureBridge;
  let wallet1: ethers.Wallet;
  let wallet2: ethers.Wallet;

  let webbRelayer: WebbRelayer;

  before(async () => {
    const pkg = await import('get-port');
    const getPort = pkg.default;
    const portNumbers = pkg.portNumbers;

    // first we need to start local evm node.
    const localChain1Port = await getPort({ port: portNumbers(3333, 4444) });
    localChain1 = new LocalChain('TestA', localChain1Port, [
      {
        secretKey: PK1,
        balance: ethers.utils.parseEther('1000').toHexString(),
      },
    ]);

    const localChain2Port = await getPort({ port: portNumbers(3333, 4444) });
    localChain2 = new LocalChain('TestB', localChain2Port, [
      {
        secretKey: PK2,
        balance: ethers.utils.parseEther('1000').toHexString(),
      },
    ]);

    wallet1 = new ethers.Wallet(PK1, localChain1.provider());
    wallet2 = new ethers.Wallet(PK2, localChain2.provider());
    // Deploy the token.
    const localToken1 = await localChain1.deployToken(
      'Webb Token',
      'WEBB',
      wallet1
    );
    const localToken2 = await localChain2.deployToken(
      'Webb Token',
      'WEBB',
      wallet2
    );

    signatureBridge = await localChain1.deploySignatureBridge(
      localChain2,
      localToken1,
      localToken2,
      wallet1,
      wallet2
    );
    // save the chain configs.
    await localChain1.writeConfig(
      `${tmpDirPath}/${localChain1.name}.json`,
      signatureBridge
    );
    await localChain2.writeConfig(
      `${tmpDirPath}/${localChain2.name}.json`,
      signatureBridge
    );

    // get the anhor on localchain1
    const anchor = signatureBridge.getAnchor(
      localChain1.chainId,
      ethers.utils.parseEther('1')
    )!;
    await anchor.setSigner(wallet1);
    // approve token spending
    const tokenAddress = signatureBridge.getWebbTokenAddress(
      localChain1.chainId
    )!;
    const token = await Tokens.MintableToken.tokenFromAddress(
      tokenAddress,
      wallet1
    );
    await token.approveSpending(anchor.contract.address);
    await token.mintTokens(wallet1.address, ethers.utils.parseEther('1000'));

    // do the same but on localchain2
    const anchor2 = signatureBridge.getAnchor(
      localChain2.chainId,
      ethers.utils.parseEther('1')
    )!;
    await anchor2.setSigner(wallet2);
    const tokenAddress2 = signatureBridge.getWebbTokenAddress(
      localChain2.chainId
    )!;
    const token2 = await Tokens.MintableToken.tokenFromAddress(
      tokenAddress2,
      wallet2
    );

    await token2.approveSpending(anchor2.contract.address);
    await token2.mintTokens(wallet2.address, ethers.utils.parseEther('1000'));

    // now start the relayer
    const relayerPort = await getPort({ port: portNumbers(9955, 9999) });
    webbRelayer = new WebbRelayer({
      port: relayerPort,
      tmp: true,
      configDir: tmpDirPath,
    });
    await webbRelayer.waitUntilReady();
  });

  it('should relay same transaction on same chain', async () => {
    // we will use chain1 as an example here.
    const anchor1 = signatureBridge.getAnchor(
      localChain1.chainId,
      ethers.utils.parseEther('1')
    );
    await anchor1.setSigner(wallet1);
    const tokenAddress = signatureBridge.getWebbTokenAddress(
      localChain1.chainId
    )!;
    const token = await Tokens.MintableToken.tokenFromAddress(
      tokenAddress,
      wallet1
    );
    const webbBalance = await token.getBalance(wallet1.address);
    expect(webbBalance.toBigInt()).to.equal(
      ethers.utils.parseEther('1000').toBigInt()
    );
    // now we are ready to do the deposit.
    const depositInfo = await anchor1.deposit(localChain1.chainId);
    const recipient = new ethers.Wallet(
      ethers.utils.randomBytes(32),
      localChain1.provider()
    );

    const relayerInfo = await webbRelayer.info();
    const localChain1Info = relayerInfo.evm[localChain1.chainId];
    const relayerFeePercentage =
      localChain1Info?.contracts.find(
        (c) => c.address === anchor1.contract.address
      )?.withdrawFeePercentage ?? 0;
    const withdrawalInfo = await anchor1.setupWithdraw(
      depositInfo.deposit,
      depositInfo.index,
      recipient.address,
      wallet1.address,
      calcualteRelayerFees(
        anchor1.denomination!,
        relayerFeePercentage
      ).toBigInt(),
      0
    );

    // ping the relayer!
    await webbRelayer.ping();
    // now send the withdrawal request.
    const txHash = await webbRelayer.anchorWithdraw(
      localChain1.chainId.toString(),
      anchor1.getAddress(),
      `0x${withdrawalInfo.proofEncoded}`,
      withdrawalInfo.publicInputs,
      withdrawalInfo.extData
    );
    expect(txHash).to.be.string;
  });

  after(async () => {
    await localChain1.stop();
    await localChain2.stop();
    await webbRelayer.stop();
    tmp.cleanupSync(); // clean up the temp dir.
  });
});
