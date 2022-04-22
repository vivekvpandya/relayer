# Transaction Relayer
### Why we need it?
In asset mixer based privacy preserving solution on withdrawal of asset if transaction fees are paid from user's account then user's private address can be related to its known address.
To prevent above issue relayer can submit a withdrawal transaction on behalf of user to asset mixer system.

### How it works?
The relayer will receive withdrawal transaction related information (via some privacy preserving solution like Tor). Based on all provided information the relayer will make withdrawal transaction. 
The relayer will pay gas fee for withdrawal transaction, which will be deducted from actual amount returned to user from the mixer system.
So say if user have deposited 100 ETH, user will get back 100 - GAS ETH where GAS = withdrawal tx fee + fee charged by mixer system.

### Code in this directory
This directory contains sub-directory for various blockchains for which Webb implements asset mixer protocols. For each protocol a relay_tx function is implemented which validates certain information and then makes connection to required chain (node) and submits withdraw transaction and observes progress of transaction and keep updating the status based on observation.
The function defined in tx_relay module will be executed by handler part of relayer when it receives command for it. 
