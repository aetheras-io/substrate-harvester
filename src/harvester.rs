use crate::metadata::Metadata;
use crate::traits::BlockProcessor;

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::{
    future::{self, Either},
    pin_mut, StreamExt,
};
use futures_timer::Delay;

use sp_arithmetic::traits::One;
use sp_runtime::{
    generic::BlockId,
    traits::{Block as BlockT, Header as HeaderT, NumberFor},
};
use sp_version::RuntimeVersion;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Block subscription terminated")]
    Subscription,
    #[error("Client Blockchain error: {0}")]
    Blockchain(#[from] sp_blockchain::Error),
    #[error("Client CallApiAt error: {0}")]
    CallApAt(String),
    #[error("Client ProvideRuntimeApi error: {0}")]
    ProvideRuntimeApi(String),
    #[error("Codec error: {0}")]
    Codec(#[from] codec::Error),
    #[error("Metadata error: {0}")]
    Metadata(#[from] crate::metadata::Error),
    #[error("Block hash not found for block number")]
    BlockHashNotFound,
    #[error("Client error {0:?}")]
    Client(String),
    #[error("Block Processor error: {0}")]
    BlockProcessor(String),
}

/// Start harvesting
pub async fn start<Block: BlockT, Client, BP>(
    client: Client,
    block_processor: BP,
) -> Result<(), Error>
where
    BP: BlockProcessor<Block, Client>,
    Client: crate::traits::MinimalClient<Block> + Send + Sync,
{
    // TODO: This should be configurable
    let finalize_only = false;

    // Should the two modes share the same init sequence?  Consider pulling some parts into this function

    if finalize_only {
        harvest_finalize_only(client, block_processor).await?;
    } else {
        harvest_all(client, block_processor).await?;
    }

    Err(Error::Subscription)
}

async fn harvest_all<Block: BlockT, Client, BP>(
    client: Client,
    block_processor: BP,
) -> Result<(), Error>
where
    BP: BlockProcessor<Block, Client>,
    Client: crate::traits::MinimalClient<Block> + Send + Sync,
{
    let client = Arc::new(client);

    let mut last_finalized: NumberFor<Block> = block_processor
        .last_finalized_block()
        .map_err(|e| Error::BlockProcessor(e.to_string()))?;

    let mut finalized_block = client
        .finalized_head()
        .await
        .map_err(|e| Error::Client(e.to_string()))?;
    let (metadata, runtime) = chain_info_at(client.clone(), finalized_block).await?;
    let block_processor = Arc::new(block_processor);

    // Catch up on finalized block if it's too far behind
    loop {
        if finalized_block - last_finalized < 10.into() {
            break;
        }

        log::info!(
            "🎥 Catching up required.  Last Finalized Block: {:?}, Tip Finalized: {:?}",
            last_finalized,
            finalized_block
        );

        // do catch up
        let result = block_processor
            .try_finalize(&*client, finalized_block)
            .await
            .map_err(|e| Error::Client(e.to_string()))?;

        if let Some(start) = result {
            repair_range(
                client.clone(),
                &metadata,
                &runtime,
                block_processor.clone(),
                start,
                finalized_block,
            )
            .await?;
        }

        last_finalized = block_processor
            .last_finalized_block()
            .map_err(|e| Error::BlockProcessor(e.to_string()))?;

        finalized_block = client
            .finalized_head()
            .await
            .map_err(|e| Error::Client(e.to_string()))?;
    }

    let mut finality_notification_stream = client
        .finality_notification_stream()
        .await
        .fuse()
        .peekable();
    let mut import_notification_stream =
        client.import_notification_stream().await.fuse().peekable();

    // peek one block to see where the chain is currently at
    let pinned_stream = Pin::new(&mut finality_notification_stream);
    if let Some(block) = pinned_stream.peek().await {
        let new_block = *block.header.number();

        log::info!("🎥 Peeked block number: {:?}", new_block);
        if last_finalized + One::one() < new_block {
            log::info!(
                "🎥 Catching up required.  Last Block: {:?}, Tip: {:?}",
                last_finalized,
                new_block
            );
            // Migrate pending blocks if possible
            let result = block_processor
                .try_finalize(&*client, new_block - One::one())
                .await
                .map_err(|e| Error::Client(e.to_string()))?;

            if let Some(start) = result {
                repair_range(
                    client.clone(),
                    &metadata,
                    &runtime,
                    block_processor.clone(),
                    start,
                    new_block - One::one(),
                )
                .await?;
            }
        } else if last_finalized > new_block {
            // can this happen?
            log::warn!(
                "🎥❓ Processed block {:?} ahead of block stream item {:?}?????!",
                last_finalized,
                new_block,
            );
        }
    }

    // Race between finality stream and pending stream
    loop {
        let outcome = {
            let next_final_block = finality_notification_stream.next();
            let next_block = import_notification_stream.next();
            pin_mut!(next_final_block, next_block);
            match future::select(next_final_block, next_block).await {
                Either::Left((v, _)) => Either::Left(v),
                Either::Right((v, _)) => Either::Right(v),
            }
        };

        match outcome {
            // 📗 Finality Stream closed
            Either::Left(None) => return Err(Error::Subscription),

            // 📗 Finality Block
            Either::Left(Some(final_block)) => {
                let current_block = *final_block.header.number();

                log::info!("📗 Received finalized block: {:?}", current_block);

                // Migrate pending blocks if possible
                let result = match block_processor.try_finalize(&*client, current_block).await {
                    Ok(r) => r,
                    Err(e) => {
                        println!("📗❗️ Error while try_finalizing");
                        println!("{:?}", e);
                        break;
                    }
                };

                // There were invalid pending blocks or were unavailable
                if let Some(start) = result {
                    repair_range(
                        client.clone(),
                        &metadata,
                        &runtime,
                        block_processor.clone(),
                        start,
                        current_block,
                    )
                    .await?;
                }
            }

            // 📘 import_notification_stream closed
            Either::Right(None) => return Err(Error::Subscription),

            // 📘 Pending Block
            Either::Right(Some(block)) => {
                let current_block = *block.header.number();
                let last_processed = block_processor
                    .last_pending_block()
                    .map_err(|e| Error::BlockProcessor(e.to_string()))?;

                log::info!("📘 Received Pending Block: {:?}", current_block);

                if last_processed >= current_block {
                    log::info!("📘❓ Block has already been handled. Skipping");
                    continue;
                }

                process_pending_range(
                    client.clone(),
                    &metadata,
                    &runtime,
                    block_processor.clone(),
                    last_processed + One::one(),
                    current_block,
                )
                .await?;
            }
        }
    }

    Err(Error::Subscription)
}

/// Process finalized blocks only
async fn harvest_finalize_only<Block: BlockT, Client, BP>(
    client: Client,
    block_processor: BP,
) -> Result<(), Error>
where
    BP: BlockProcessor<Block, Client>,
    Client: crate::traits::MinimalClient<Block> + Send + Sync,
{
    let client = Arc::new(client);

    let last_finalized: NumberFor<Block> = block_processor
        .last_finalized_block()
        .map_err(|e| Error::BlockProcessor(e.to_string()))?;

    let finalized_block = client
        .finalized_head()
        .await
        .map_err(|e| Error::Client(e.to_string()))?;
    let (metadata, runtime) = chain_info_at(client.clone(), finalized_block).await?;
    let block_processor = Arc::new(block_processor);
    let mut finality_notification_stream = client
        .finality_notification_stream()
        .await
        .fuse()
        .peekable();

    // Purge all pending blocks
    block_processor
        .purge_pending(last_finalized)
        .map_err(|e| Error::BlockProcessor(e.to_string()))?;

    // peek one block to see where the chain is currently at
    let pinned_stream = Pin::new(&mut finality_notification_stream);
    if let Some(block) = pinned_stream.peek().await {
        let new_block = *block.header.number();
        log::info!("📗 Peeked block number: {:?}", new_block);
        if last_finalized + One::one() < new_block {
            log::info!(
                "📗 Catching up required.  Last Block: {:?}, Tip: {:?}",
                last_finalized,
                new_block
            );
            repair_range(
                client.clone(),
                &metadata,
                &runtime,
                block_processor.clone(),
                last_finalized + One::one(),
                new_block - One::one(),
            )
            .await?;
        } else if last_finalized > new_block {
            // can this happen?
            log::warn!(
                "📗 Processed block {:?} ahead of block stream item {:?}?????!",
                last_finalized,
                new_block,
            );
        }
    }

    while let Some(block) = Pin::new(&mut finality_notification_stream).next().await {
        let current_block = *block.header.number();

        // do block handling....
        log::info!("📗 Received finalized block: {:?}", current_block);

        let last_finalized: NumberFor<Block> = block_processor
            .last_finalized_block()
            .map_err(|e| Error::BlockProcessor(e.to_string()))?;

        // There were invalid pending blocks or were unavailable
        repair_range(
            client.clone(),
            &metadata,
            &runtime,
            block_processor.clone(),
            last_finalized + One::one(),
            current_block,
        )
        .await?;
    }
    Err(Error::Subscription)
}

/// Pulls and handles finalized blocks only
async fn repair_range<Block: BlockT, Client, BP>(
    client: Arc<Client>,
    metadata: &Metadata,
    runtime: &RuntimeVersion,
    block_processor: Arc<BP>,
    start: NumberFor<Block>,
    end: NumberFor<Block>,
) -> Result<NumberFor<Block>, Error>
where
    BP: BlockProcessor<Block, Client>,
    Client: crate::traits::MinimalClient<Block> + Send + Sync,
{
    const PROCESSING_PACE: Duration = Duration::from_millis(100); // roughly 10 blocks per second

    let mut next_block = start;
    let mut metadata = metadata.clone();
    let mut runtime = runtime.clone();
    let mut delay_wait = Delay::new(Duration::new(0, 0)); // fire off right away
    log::info!("📗 Finalizing range {:?}-{:?}", start, end);

    loop {
        delay_wait.await;

        let block_id = BlockId::<Block>::number(next_block);
        let runtime_at = client
            .runtime_version_at(Some(block_id))
            .await
            .map_err(|e| Error::Client(e.to_string()))?;

        if runtime.spec_version != runtime_at.spec_version {
            // rolling back to old metadata and runtime version
            runtime = runtime_at;
            metadata = client
                .metadata_at(Some(block_id))
                .await
                .map_err(|e| Error::Client(e.to_string()))?;
        }

        log::info!("📗 Processing block: {:?}", next_block);

        if let Err(e) = block_processor
            .handle_final(&metadata, &*client, next_block)
            .await
        {
            log::error!(
                "Failed to repair.  Stopped at: {:?}. Error message: {:?}",
                next_block,
                e.to_string()
            );
            return Ok(next_block - One::one());
        }

        next_block += One::one();
        if next_block > end {
            log::info!("📗 Finished repair range {:?}-{:?}", start, end);
            return Ok(end);
        }

        delay_wait = Delay::new(PROCESSING_PACE);
    }
}

async fn process_pending_range<Block: BlockT, Client, BP>(
    client: Arc<Client>,
    metadata: &Metadata,
    runtime: &RuntimeVersion,
    block_processor: Arc<BP>,
    start: NumberFor<Block>,
    end: NumberFor<Block>,
) -> Result<NumberFor<Block>, Error>
where
    BP: BlockProcessor<Block, Client>,
    Client: crate::traits::MinimalClient<Block> + Send + Sync,
{
    const PROCESSING_PACE: Duration = Duration::from_millis(100); // roughly 10 blocks per second

    let mut next_block = start;
    let mut metadata = metadata.clone();
    let mut runtime = runtime.clone();
    let mut delay_wait = Delay::new(Duration::new(0, 0)); // fire off right away
    log::info!("📘 Processing pending range {:?}-{:?}", start, end);

    loop {
        delay_wait.await;

        let block_id = BlockId::<Block>::number(next_block);
        let runtime_at = client
            .runtime_version_at(Some(block_id))
            .await
            .map_err(|e| Error::Client(e.to_string()))?;

        if runtime.spec_version != runtime_at.spec_version {
            // rolling back to old metadata and runtime version
            runtime = runtime_at;
            metadata = client
                .metadata_at(Some(block_id))
                .await
                .map_err(|e| Error::Client(e.to_string()))?;
        }

        log::info!("📘 Processing block: {:?}", next_block);
        if let Err(e) = block_processor
            .handle_pending(&metadata, &*client, next_block)
            .await
        {
            log::error!(
                "❗️Failed to process.  Stopped at: {:?}, Error: {:?}",
                next_block,
                e
            );
            return Ok(next_block - One::one());
        }

        next_block += One::one();
        if next_block > end {
            log::info!("📘 Finished processing pending range");
            return Ok(end);
        }

        delay_wait = Delay::new(PROCESSING_PACE);
    }
}

async fn chain_info_at<Block: BlockT, Client>(
    client: Arc<Client>,
    block_number: NumberFor<Block>,
) -> Result<(Metadata, RuntimeVersion), Error>
where
    Client: crate::traits::MinimalClient<Block> + Send + Sync,
{
    let block_id = BlockId::<Block>::number(block_number);

    let runtime = client
        .runtime_version_at(Some(block_id))
        .await
        .map_err(|e| Error::Client(e.to_string()))?;
    let metadata = client
        .metadata_at(Some(block_id))
        .await
        .map_err(|e| Error::Client(e.to_string()))?;
    Ok((metadata, runtime))
}
