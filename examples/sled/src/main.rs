use reth_db::{cursor::DbCursorRO, open_db_read_only, table::Compress, tables, transaction::DbTx};
use reth_primitives::{
    keccak256, revm_primitives::FixedBytes, Address, ChainSpecBuilder, TransactionSignedNoHash,
    B256,
};
use reth_provider::{
    AccountReader, BlockReader, BlockSource, DatabaseProviderFactory, HeaderProvider,
    ProviderFactory, ReceiptProvider, StateProvider, TransactionsProvider,
};
use reth_rpc_types::{Filter, FilteredParams};
use std::path::Path;

// in reth: 33781302 accounts
// in sled: 33781302 accounts
//
// reth size:
//  | PlainAccountState          | 33781302  | 6340         | 664574     | 0              | 2.6 GiB    |
//
// sled size:
//  ❯ du -sh reth
//  785M	reth
//
// todo: check if diff is mostly because data was inserted sorted into sled
fn main() -> eyre::Result<()> {
    println!("open");

    // open reth db
    let db_path = std::env::var("RETH_DB_PATH")?;
    let db_path = Path::new(&db_path);
    let db = open_db_read_only(db_path.join("db").as_path(), Default::default())?;
    let spec = ChainSpecBuilder::mainnet().build();
    let factory = ProviderFactory::new(db, spec.into(), db_path.join("static_files"))?;

    // open sled
    let sled = sled::open("reth").expect("could not open sled");

    // open ro tx
    let provider = factory.provider()?.disable_long_read_transaction_safety();

    // migrate tx's
    let account_tree = sled.open_tree("Accounts").expect("could not open tx tree");
    println!("entries: {}", account_tree.len());
    return Ok(());
    for item in account_tree.iter() {
        let item = item.unwrap();
        println!(
            "existing item: {}",
            Address::from_word(FixedBytes::from(
                TryInto::<[u8; 32]>::try_into(item.0.as_ref()).unwrap()
            ))
        );
    }
    let tx = provider.into_tx();
    let mut cursor =
        tx.cursor_read::<tables::PlainAccountState>().expect("could not open acc cursor");
    for item in cursor.walk_range(Address::ZERO..).expect("could not open walker") {
        let (address, account) = item.expect("db read error");
        println!("writing account {address}");
        account_tree
            .insert(address.as_slice(), account.clone().compress())
            .expect("could not insert acc");
        println!("wrote account");
    }
    println!("wrote accounts");

    account_tree.flush().unwrap();
    println!("flushed");

    Ok(())
}

/// The `HeaderProvider` allows querying the headers-related tables.
fn header_provider_example<T: HeaderProvider>(provider: T, number: u64) -> eyre::Result<()> {
    // Can query the header by number
    let header = provider.header_by_number(number)?.ok_or(eyre::eyre!("header not found"))?;

    // We can convert a header to a sealed header which contains the hash w/o needing to re-compute
    // it every time.
    let sealed_header = header.seal_slow();

    // Can also query the header by hash!
    let header_by_hash =
        provider.header(&sealed_header.hash())?.ok_or(eyre::eyre!("header by hash not found"))?;
    assert_eq!(sealed_header.header(), &header_by_hash);

    // The header's total difficulty is stored in a separate table, so we have a separate call for
    // it. This is not needed for post PoS transition chains.
    let td = provider.header_td_by_number(number)?.ok_or(eyre::eyre!("header td not found"))?;
    assert!(!td.is_zero());

    // Can query headers by range as well, already sealed!
    let headers = provider.sealed_headers_range(100..200)?;
    assert_eq!(headers.len(), 100);

    Ok(())
}

/// The `TransactionsProvider` allows querying transaction-related information
fn txs_provider_example<T: TransactionsProvider>(provider: T) -> eyre::Result<()> {
    // Try the 5th tx
    let txid = 5;

    // Query a transaction by its primary ordered key in the db
    let tx = provider.transaction_by_id(txid)?.ok_or(eyre::eyre!("transaction not found"))?;

    // Can query the tx by hash
    let tx_by_hash =
        provider.transaction_by_hash(tx.hash)?.ok_or(eyre::eyre!("txhash not found"))?;
    assert_eq!(tx, tx_by_hash);

    // Can query the tx by hash with info about the block it was included in
    let (tx, meta) =
        provider.transaction_by_hash_with_meta(tx.hash)?.ok_or(eyre::eyre!("txhash not found"))?;
    assert_eq!(tx.hash, meta.tx_hash);

    // Can reverse lookup the key too
    let id = provider.transaction_id(tx.hash)?.ok_or(eyre::eyre!("txhash not found"))?;
    assert_eq!(id, txid);

    // Can find the block of a transaction given its key
    let _block = provider.transaction_block(txid)?;

    // Can query the txs in the range [100, 200)
    let _txs_by_tx_range = provider.transactions_by_tx_range(100..200)?;
    // Can query the txs in the _block_ range [100, 200)]
    let _txs_by_block_range = provider.transactions_by_block_range(100..200)?;

    Ok(())
}

/// The `BlockReader` allows querying the headers-related tables.
fn block_provider_example<T: BlockReader>(provider: T, number: u64) -> eyre::Result<()> {
    // Can query a block by number
    let block = provider.block(number.into())?.ok_or(eyre::eyre!("block num not found"))?;
    assert_eq!(block.number, number);

    // Can query a block with its senders, this is useful when you'd want to execute a block and do
    // not want to manually recover the senders for each transaction (as each transaction is
    // stored on disk with its v,r,s but not its `from` field.).
    let block = provider.block(number.into())?.ok_or(eyre::eyre!("block num not found"))?;

    // Can seal the block to cache the hash, like the Header above.
    let sealed_block = block.clone().seal_slow();

    // Can also query the block by hash directly
    let block_by_hash = provider
        .block_by_hash(sealed_block.hash())?
        .ok_or(eyre::eyre!("block by hash not found"))?;
    assert_eq!(block, block_by_hash);

    // Or by relying in the internal conversion
    let block_by_hash2 = provider
        .block(sealed_block.hash().into())?
        .ok_or(eyre::eyre!("block by hash not found"))?;
    assert_eq!(block, block_by_hash2);

    // Or you can also specify the datasource. For this provider this always return `None`, but
    // the blockchain tree is also able to access pending state not available in the db yet.
    let block_by_hash3 = provider
        .find_block_by_hash(sealed_block.hash(), BlockSource::Any)?
        .ok_or(eyre::eyre!("block hash not found"))?;
    assert_eq!(block, block_by_hash3);

    // Can query the block's ommers/uncles
    let _ommers = provider.ommers(number.into())?;

    // Can query the block's withdrawals (via the `WithdrawalsProvider`)
    let _withdrawals =
        provider.withdrawals_by_block(sealed_block.hash().into(), sealed_block.timestamp)?;

    Ok(())
}

/// The `ReceiptProvider` allows querying the receipts tables.
fn receipts_provider_example<T: ReceiptProvider + TransactionsProvider + HeaderProvider>(
    provider: T,
) -> eyre::Result<()> {
    let txid = 5;
    let header_num = 100;

    // Query a receipt by txid
    let receipt = provider.receipt(txid)?.ok_or(eyre::eyre!("tx receipt not found"))?;

    // Can query receipt by txhash too
    let tx = provider.transaction_by_id(txid)?.unwrap();
    let receipt_by_hash =
        provider.receipt_by_hash(tx.hash)?.ok_or(eyre::eyre!("tx receipt by hash not found"))?;
    assert_eq!(receipt, receipt_by_hash);

    // Can query all the receipts in a block
    let _receipts = provider
        .receipts_by_block(100.into())?
        .ok_or(eyre::eyre!("no receipts found for block"))?;

    // Can check if a address/topic filter is present in a header, if it is we query the block and
    // receipts and do something with the data
    // 1. get the bloom from the header
    let header = provider.header_by_number(header_num)?.unwrap();
    let bloom = header.logs_bloom;

    // 2. Construct the address/topics filters
    // For a hypothetical address, we'll want to filter down for a specific indexed topic (e.g.
    // `from`).
    let addr = Address::random();
    let topic = B256::random();

    // TODO: Make it clearer how to choose between event_signature(topic0) (event name) and the
    // other 3 indexed topics. This API is a bit clunky and not obvious to use at the moemnt.
    let filter = Filter::new().address(addr).event_signature(topic);
    let filter_params = FilteredParams::new(Some(filter));
    let address_filter = FilteredParams::address_filter(&addr.into());
    let topics_filter = FilteredParams::topics_filter(&[topic.into()]);

    // 3. If the address & topics filters match do something. We use the outer check against the
    // bloom filter stored in the header to avoid having to query the receipts table when there
    // is no instance of any event that matches the filter in the header.
    if FilteredParams::matches_address(bloom, &address_filter) &&
        FilteredParams::matches_topics(bloom, &topics_filter)
    {
        let receipts = provider.receipt(header_num)?.ok_or(eyre::eyre!("receipt not found"))?;
        for log in &receipts.logs {
            if filter_params.filter_address(&log.address) &&
                filter_params.filter_topics(log.topics())
            {
                // Do something with the log e.g. decode it.
                println!("Matching log found! {log:?}")
            }
        }
    }

    Ok(())
}

fn state_provider_example<T: StateProvider + AccountReader>(provider: T) -> eyre::Result<()> {
    let address = Address::random();
    let storage_key = B256::random();

    // Can get account / storage state with simple point queries
    let _account = provider.basic_account(address)?;
    let _code = provider.account_code(address)?;
    let _storage = provider.storage(address, storage_key)?;
    // TODO: unimplemented.
    // let _proof = provider.proof(address, &[])?;

    Ok(())
}
