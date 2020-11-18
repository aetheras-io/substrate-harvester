use crate::metadata::Metadata;
use crate::storage_key_at;
use crate::traits::{BlockProcessor, Decoder, EventExtractor, IndexStore, MinimalClient};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::marker::PhantomData;

// use codec::{Decode, Encode};
use sp_arithmetic::traits::{AtLeast32Bit, One};
use sp_core::storage::StorageKey;
use sp_runtime::{
    generic::BlockId,
    traits::{Block as BlockT, Header as HeaderT, NumberFor},
};

// use frame_support::traits::Currency;

// use frame_system::{Phase, Trait as System};
// use pallet_balances::Trait as Balances;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Sqlite error: {0:?}")]
    Sqlite(String),
    #[error("Client error {0:?}")]
    Client(String),
    #[error("Decoder error {0:?}")]
    Decoder(String),    
    #[error("Other: {0:?}")]
    Other(String),
}

#[derive(Debug)]
pub struct Indexer<Decode, S, Block, Client, Extractor, RuntimeEvent, E> {
    decoder: Decode,
    events_store_key: StorageKey,
    store: S,
    _marker: PhantomData<(Block, Client, Extractor, RuntimeEvent, E)>,
}

impl<Decode, S, Block: BlockT, Client, Extractor, RuntimeEvent, E>
    Indexer<Decode, S, Block, Client, Extractor, RuntimeEvent, E>
where
    S: IndexStore<RuntimeEvent, E> + Send + Sync,
    Decode: Decoder<RuntimeEvent> + Send + Sync,
    Client: MinimalClient<Block> + Send + Sync,
    Extractor: EventExtractor<Block, Client, RuntimeEvent, E> + Send + Sync,
    RuntimeEvent: Send + Sync,
{
    pub fn new(store: S, decoder: Decode) -> Self {
        Self {
            decoder,
            events_store_key: storage_key_at(b"System", b"Events"),
            store,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<Decode, S, Block: BlockT, Client, Extractor, RuntimeEvent, E> BlockProcessor<Block, Client>
    for Indexer<Decode, S, Block, Client, Extractor, RuntimeEvent, E>
where
    S: IndexStore<RuntimeEvent, E> + Sync + Send,
    Decode: Decoder<RuntimeEvent> + Send + Sync,
    Client: MinimalClient<Block> + Sync + Send,
    Extractor: EventExtractor<Block, Client, RuntimeEvent, E> + Send + Sync,
    E: Send + Sync,
    RuntimeEvent: Send + Sync,
    <Block::Header as HeaderT>::Number: AtLeast32Bit + Into<u32>,
{
    type Error = Error;

    async fn handle_pending(
        &self,
        metadata: &Metadata,
        client: &Client,
        block: NumberFor<Block>,
    ) -> Result<(), Self::Error> {
        // #TODO Need testing to see what happens to substrate's finality notification if the chain itself is
        // still synchronizing to the network and doesn't actually have the block.
        let block_data = client
            .storage(BlockId::<Block>::Number(block), &self.events_store_key)
            .await
            .map_err(|e| Error::Client(e.to_string()))?;

        let hash = client
            .hash(block)
            .await
            .map_err(|e| Error::Client(e.to_string()))?
            .expect("Failed to retrieve hash")
            .as_ref()
            .to_vec();

        if let Some(entry) = block_data {
            // TODO: Handle Parse event errors

            let records = stream::iter(
                self.decoder
                    .decode_events(metadata, &mut entry.0.as_slice())
                    .map_err(|e| Error::Decoder(e.to_string()))?,
            )
            .then(|(phase, runtime_event)| async move {
                let data = Extractor::extract(client, block, &phase, &runtime_event)
                    .await
                    .unwrap();

                (phase, runtime_event, data)
            })
            .collect()
            .await;

            self.store
                .process_pending(block.into(), hash, records)
                .map_err(|e| Error::Sqlite(e.to_string()))?;
        } else {
            log::warn!("❗️ Block {:?} has no events.  This is unusual.", block);
        }
        Ok(())
    }

    async fn handle_final(
        &self,
        metadata: &Metadata,
        client: &Client,
        block: NumberFor<Block>,
    ) -> Result<(), Self::Error> {
        // #TODO Need testing to see what happens to substrate's finality notification if the chain itself is
        // still synchronizing to the network and doesn't actually have the block.
        let block_data = client
            .storage(BlockId::<Block>::Number(block), &self.events_store_key)
            .await
            .map_err(|e| Error::Client(e.to_string()))?;

        if let Some(entry) = block_data {
            // TODO: Handle Parse event errors
            let records = stream::iter(
                self.decoder
                    .decode_events(metadata, &mut entry.0.as_slice())
                    .map_err(|e| Error::Decoder(e.to_string()))?,
            )
            .then(|(phase, runtime_event)| async move {
                let data = Extractor::extract(client, block, &phase, &runtime_event)
                    .await
                    .unwrap();

                (phase, runtime_event, data)
            })
            .collect()
            .await;

            self.store
                .process_finalized(block.into(), records)
                .map_err(|e| Error::Sqlite(e.to_string()))?;
        } else {
            log::warn!("❗️ Block {:?} has no events.  This is unusual.", block);
        }

        Ok(())
    }

    async fn try_finalize(
        &self,
        client: &Client,
        block: NumberFor<Block>,
    ) -> Result<Option<NumberFor<Block>>, Self::Error> {
        let pending_blocks = self
            .store
            .non_finalized_blocks()
            .map_err(|e| Error::Sqlite(e.to_string()))?;
        let mut idx = 0;
        let mut current_block = self.last_finalized_block()? + One::one();
        let end_block = block;
        log::info!(
            "📘 -> 📗 Migrating pending blocks from {:?}-{:?}",
            current_block,
            end_block
        );

        loop {
            if current_block > end_block {
                break;
            }
            let finalized_hash = client
                .hash(current_block)
                .await
                .map_err(|e| Error::Client(e.to_string()))?
                .expect("Failed to retrieve block hash from client");

            // We've exhausted pending blocks
            if idx >= pending_blocks.len() {
                return Ok(Some(current_block));
            }

            let pending_block = pending_blocks[idx].to_owned();
            let pending_block_num: NumberFor<Block> = pending_block.0.into();

            if pending_block_num != current_block {
                // Mismatch, this should never happen. Clear everything
                // Set pending counter to current
                let prev_block_num = current_block - One::one();
                self.store
                    .purge_pending(prev_block_num.into())
                    .map_err(|e| Error::Sqlite(e.to_string()))?;
                log::info!("📗❓ No matching pending block for: {:?}", current_block);
                return Ok(Some(current_block));
            }

            let cached_hash = pending_block.1.unwrap();

            if &cached_hash[..] != finalized_hash.as_ref() {
                // Set pending counter to current and clear
                let prev_block_num = current_block - One::one();
                self.store
                    .purge_pending(prev_block_num.into())
                    .map_err(|e| Error::Sqlite(e.to_string()))?;
                log::warn!("📗❗️ Matching block number with pending block, but different hash!");
                return Ok(Some(current_block));
            }

            self.store
                .finalize(current_block.into())
                .map_err(|e| Error::Sqlite(e.to_string()))?;

            idx += 1;
            current_block += One::one();
        }
        log::info!("📗 Finished migrating blocks ✅");
        Ok(None)
    }

    fn last_finalized_block(&self) -> Result<NumberFor<Block>, Self::Error> {
        let block: NumberFor<Block> = self
            .store
            .last_finalized_block()
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .into();
        Ok(block)
    }

    fn last_pending_block(&self) -> Result<NumberFor<Block>, Self::Error> {
        let block: NumberFor<Block> = self
            .store
            .last_pending_block()
            .map_err(|e| Error::Sqlite(e.to_string()))?
            .into();
        Ok(block)
    }

    fn purge_pending(&self, block: NumberFor<Block>) -> Result<(), Self::Error> {
        self.store
            .purge_pending(block.into())
            .map_err(|e| Error::Sqlite(e.to_string()))
    }
}
