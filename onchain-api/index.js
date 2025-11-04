const {
  Connection,
  PublicKey,
  Keypair,
  Transaction,
  TransactionInstruction,
  SystemProgram,
  sendAndConfirmTransaction,
  LAMPORTS_PER_SOL,
} = require("@solana/web3.js");
const { BinaryReader, BinaryWriter, serialize, deserialize } = require("borsh");
const { deserializeMetadata } = require("@metaplex-foundation/mpl-token-metadata");
const { Buffer } = require("buffer");
const fs = require("fs");
const BN = require("bn.js");

const { PositionAccountData, InitializePositionData, ClosePositionData } = require('./schema');
const PROGRAM_ID        = new PublicKey("URAa3qGD1qVKKqyQrF8iBVZRTwa4Q8RkMd6Gx7u2KL1");
const DEX_PUBKEY        = new PublicKey("URAbknhQPhFiY92S5iM9nhzoZC5Vkch7S5VERa4PmuV");
const DEX_FEES_PUBKEY   = new PublicKey("URAfeAaGMoavvTe8vqPwMX6cUvTjq8WMG5c9nFo7Q8j");

function lamportsToSOL(lamports) {
    return lamports / LAMPORTS_PER_SOL;
}

function stringToFixedArray(str, length = 32) {
    const arr = new Array(length).fill(0);
    
    if (typeof str === 'string') {
        const bytes = new TextEncoder().encode(str);
        const copyLength = Math.min(bytes.length, length - 1);
        
        for (let i = 0; i < copyLength; i++) {
            arr[i] = bytes[i];
        }
    }
    
    return arr;
}

function getMetadataPDA(mint) {
    const [metadataPDA, metadataBump] = PublicKey.findProgramAddressSync(
        [
            new TextEncoder().encode("metadata"),
            new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s").toBytes(),
            mint.toBytes(),
        ],
        new PublicKey("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s")
    );
    return metadataPDA;
}

async function getMetadata(connection, pda) {
    const metadataAccount = await connection.getParsedAccountInfo(pda);
    
    if (!metadataAccount) {
        throw new Error('Metadata account not found');
    }

    if (!metadataAccount.value) {
        throw new Error('Metadata account value is null');
    }

    const metadata = deserializeMetadata(metadataAccount.value);
    return metadata;
}

function getMarketAccount(mint) {
  const [marketPDA, marketBump] = PublicKey.findProgramAddressSync(
    [
      new TextEncoder().encode("uranus_market"),
      mint.toBytes(),
      new TextEncoder().encode("v1"),
    ],
    PROGRAM_ID
  );
  return marketPDA;
}

async function getMarketLiquidity(connection, mint){
    const marketAccount = getMarketAccount(mint);
    const accountInfo = await connection.getAccountInfo(marketAccount);
    if (accountInfo === null) {
        throw new Error('Market account not found');
    }

    const liquidity = lamportsToSOL(accountInfo.lamports);
    return liquidity;
}

async function calculateFees(solAmount, leverage, connection) {
  const lamports = solAmount * LAMPORTS_PER_SOL;
  const basePaidAmount = new BN(lamports);

  const baseFee = basePaidAmount.mul(new BN(100)).div(new BN(10000)); // 1%
  const leverageFee = basePaidAmount.mul(new BN(10)).mul(new BN(leverage)).div(new BN(10000)); // 0.1% per leverage
  const percentageFee = baseFee.add(leverageFee);
  const accountFee = new BN(
    await connection.getMinimumBalanceForRentExemption(PositionAccountData.size)
  );
  
  return { basePaidAmount, percentageFee, accountFee };
}

async function createUranusPositionTransaction(connection, owner, mint, solAmount, leverage, direction) {
  if (!owner || !mint || !solAmount || !leverage || !direction) {
    throw new Error("Missing required parameters");
  }

  const metadataPDA = getMetadataPDA(mint);
  const marketMetadata = await getMetadata(connection, metadataPDA);
  if (!marketMetadata) throw new Error("Market metadata not found");
  if (!marketMetadata.symbol)
    throw new Error("Market metadata symbol not found");

  const { basePaidAmount, percentageFee, accountFee } = await calculateFees(solAmount, leverage, connection);
  const paidAmount = basePaidAmount.add(percentageFee).add(accountFee);
  const positionSize = basePaidAmount.sub(percentageFee).sub(accountFee).mul(new BN(leverage));

  const positionNonce = new BN(Date.now());
  const [positionPda] = PublicKey.findProgramAddressSync(
    [
      new TextEncoder().encode("uranus_position"),
      owner.toBytes(),
      positionNonce.toArray("le", 8),
    ],
    PROGRAM_ID
  );

  const serializedData = serialize(
    InitializePositionData.schema,
    new InitializePositionData({
      market_mint: mint.toBytes(),
      market_symbol: stringToFixedArray(marketMetadata.symbol),
      paid_amount: paidAmount,
      position_size: positionSize,
      leverage,
      position_nonce: positionNonce,
      direction: direction.toLowerCase() === "long" ? 1 : -1,
    })
  );

  const instructionData = new Uint8Array(1 + serializedData.length);
  instructionData[0] = 0;
  instructionData.set(serializedData, 1);

  const instruction = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: owner, isSigner: true, isWritable: true },
      { pubkey: owner, isSigner: false, isWritable: false },
      { pubkey: positionPda, isSigner: false, isWritable: true },
      { pubkey: getMarketAccount(mint), isSigner: false, isWritable: true },
      { pubkey: DEX_PUBKEY, isSigner: false, isWritable: true },
      { pubkey: DEX_FEES_PUBKEY, isSigner: false, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: instructionData,
  });

  const transaction = new Transaction().add(instruction);
  transaction.feePayer = owner;
  transaction.recentBlockhash = (
    await connection.getLatestBlockhash()
  ).blockhash;

  return {
    transaction,
    positionNonce: positionNonce.toNumber(),
    positionPda,
    fees: percentageFee.add(accountFee).toNumber() / LAMPORTS_PER_SOL,
    totalCost: paidAmount.toNumber() / LAMPORTS_PER_SOL,
  };
}

async function closeUranusPosition(connection, positionNonce, owner, positionPda) {
  if (!connection || !positionNonce || !owner) {
    throw new Error("Missing required parameters");
  }

  const positionNonceBN = new BN(positionNonce);

  if (!positionPda) {
    [positionPda] = PublicKey.findProgramAddressSync(
      [
        new TextEncoder().encode("uranus_position"),
        owner.toBytes(),
        positionNonceBN.toArray("le", 8),
      ],
      PROGRAM_ID
    );
  }

  const serializedData = serialize(
    ClosePositionData.schema,
    new ClosePositionData({
      close_position: true,
      position_nonce: positionNonceBN,
    })
  );

  const instructionData = new Uint8Array(1 + serializedData.length);
  instructionData[0] = 2;
  instructionData.set(serializedData, 1);

  const instruction = new TransactionInstruction({
    programId: PROGRAM_ID,
    keys: [
      { pubkey: positionPda, isSigner: false, isWritable: true },
      { pubkey: owner, isSigner: true, isWritable: true },
    ],
    data: instructionData,
  });

  const transaction = new Transaction().add(instruction);
  transaction.feePayer = owner;
  transaction.recentBlockhash = (await connection.getLatestBlockhash()).blockhash;

  return transaction;
}

function deserializePositionAccount(data) {
    let deserialized = deserialize(PositionAccountData.schema, data);

    const positionAccount = {
        owner: new PublicKey(deserialized.owner),
        market_mint: new PublicKey(deserialized.market_mint),
        market_symbol: new TextDecoder().decode(new Uint8Array(deserialized.market_symbol)).replace(/\0/g, ''),
        entry_price: Number(deserialized.entry_price) / LAMPORTS_PER_SOL,
        liquidation_price: Number(deserialized.liquidation_price) / LAMPORTS_PER_SOL,
        paid_amount: Number(deserialized.paid_amount) / LAMPORTS_PER_SOL,
        position_size: Number(deserialized.position_size) / LAMPORTS_PER_SOL,
        leverage: deserialized.leverage,
        closed: deserialized.closed,
        position_nonce: Number(deserialized.position_nonce),
        direction: deserialized.direction === 1 ? "LONG" : "SHORT",
    };

    return positionAccount;
}

async function getOpenPositions(connection, owner = null, marketMint = null, ticker = null){
  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    commitment: "confirmed",
  });

  const validAccounts = accounts.filter(({ account }) => account.data.byteLength !== 0);

  let deserializedAccounts = validAccounts.map(({ account }) => {
    return deserializePositionAccount(account.data);
  });

  if(owner && !marketMint && !ticker){
    deserializedAccounts = deserializedAccounts.filter((pos) => pos.owner.toBase58() === owner.toBase58());
  }
  else if(!owner && marketMint && !ticker){
    deserializedAccounts = deserializedAccounts.filter((pos) => pos.market_mint.toBase58() === marketMint.toBase58());
  }
  else if(!owner && !marketMint && ticker){
    deserializedAccounts = deserializedAccounts.filter((pos) => pos.market_symbol.toLowerCase().includes(ticker.toLowerCase()));
  }
  return deserializedAccounts;
}

async function getAllMarkets(connection){
  const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
    commitment: "confirmed",
  });

  const validAccounts = accounts.filter(({ account }) => account.data.byteLength === 0);
  
  const lamports = await Promise.all(
    validAccounts.map(async ({ pubkey }) => {
      const accountInfo = await connection.getAccountInfo(pubkey);
      return accountInfo;
    })
  );
  validAccounts.forEach((account, idx) => {
    account.lamports = lamports[idx] ? lamports[idx].lamports : 0;
  });

  return validAccounts.map(({ pubkey, lamports }) => ({
    market_account: pubkey.toBase58(),
    liquidity: lamportsToSOL(lamports),
  }));
}

async function getAllSignaturesForMarket(connection, marketMint, maxTimeSpan = 24, marketMintIsPDA = false){
  let marketAccount;

  if(!marketMintIsPDA)
    marketAccount = getMarketAccount(marketMint);
  else
    marketAccount = marketMint;

  let allSignatures = [];
  let before = null;
  const now = Math.floor(Date.now() / 1000);
  const maxTimeSpanSeconds = maxTimeSpan * 60 * 60;
  const cutoffTime = now - maxTimeSpanSeconds;

  while (true) {
    const options = { limit: 100 };
    if (before) {
      options.before = before;
    }

    const signatures = await connection.getSignaturesForAddress(
      marketAccount,
      options
    );
    
    let reachedCutoff = false;
    for (const sig of signatures) {
      if (sig.blockTime >= cutoffTime) {
        allSignatures.push(sig);
      } else {
        reachedCutoff = true;
        break;
      }
    }

    if (reachedCutoff || signatures.length < 100) {
      break;
    }
    
    before = signatures[signatures.length - 1].signature;
  }
  
  return allSignatures;
}

async function getParsedTransactionsForMarket(connection, signatures){
  const BATCH_SIZE = 50;
  let allTransactions = [];
  for (let i = 0; i < signatures.length; i += BATCH_SIZE) {
    const batch = signatures.slice(i, i + BATCH_SIZE);
    const transactions = await connection.getParsedTransactions(
      batch.map((sig) => sig.signature),
      { maxSupportedTransactionVersion: 0 }
    );
    allTransactions = allTransactions.concat(transactions);
  }

  const relevantTransactions = allTransactions.filter((tx) => {
    if (!tx || !tx.transaction) return false;
    return tx.transaction.message.instructions.some(
      (ix) => ix.programId.equals(PROGRAM_ID)
    );
  });

  return relevantTransactions;
}

async function getMarketVolume(connection, marketMint, maxTimeSpan = 24, marketMintIsPDA = false){
  const signatures = await getAllSignaturesForMarket(connection, marketMint, maxTimeSpan, marketMintIsPDA);
  const txns = await getParsedTransactionsForMarket(connection, signatures);
  const volume = countVolumeFromTransactions(txns);
  return volume;
}

function getOpenOrders(txns){
  let openOrders = [];
  txns.forEach((txn) => {
    const logs = txn.meta.logMessages;
    logs.forEach((log) => {
      if (log.includes("Position initialized")) {
        openOrders.push(txn);
      }
    });
  });
  return openOrders;
}

function getOrdersSOLVolume(orders) {
  let totalVolume = 0;
  orders.forEach((order) => {
    const preBalances = order.meta.preBalances;
    const postBalances = order.meta.postBalances;
    
    for (let i = 0; i < preBalances.length; i++) {
      const balanceChange = Math.abs(preBalances[i] - postBalances[i]);
      totalVolume += balanceChange;
    }
  });
  return lamportsToSOL(totalVolume);
}

function countVolumeFromTransactions(txns){
  let totalVolume = 0;
  txns.forEach((txn) => {
    const preBalance = txn.meta.preBalances[0];
    const postBalance = txn.meta.postBalances[0];
    const balanceChange = preBalance - postBalance;
    totalVolume += balanceChange;
  });
  return lamportsToSOL(totalVolume);
}

async function getTickerPrice(ticker){
  const response = await fetch(`https://core.uranus.ag/price?symbol=${ticker.toUpperCase()}`);
  
  if(!response.ok){
    throw new Error("Failed to fetch price");
  }

  const data = await response.json();
  return data.price;
}

module.exports = {
    getMarketAccount,
    getMarketLiquidity,
    calculateFees,
    createUranusPositionTransaction,
    closeUranusPosition,
    getOpenPositions,
    getAllMarkets,
    getAllSignaturesForMarket,
    getParsedTransactionsForMarket,
    getMarketVolume,
    getOpenOrders,
    getOrdersSOLVolume,
    countVolumeFromTransactions,
    getTickerPrice,
}