use crate::metadata::Metadata;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use sp_core::storage::StorageKey;
use sp_runtime::{
    generic::BlockId,
    traits::{Block as BlockT, NumberFor},
};
use sp_storage::StorageData;
use sp_version::RuntimeVersion;

use frame_system::Phase;

pub struct BlockImportEvent<Block: BlockT> {
    pub header: Block::Header,
}

pub struct FinalityImportEvent<Block: BlockT> {
    pub header: Block::Header,
}

#[async_trait]
pub trait BlockProcessor<Block: BlockT, Client> {
    type Error: std::error::Error + Send + 'static;

    async fn handle_pending(
        &self,
        _metadata: &Metadata,
        _client: &Client,
        _block: NumberFor<Block>,
    ) -> Result<(), Self::Error>;

    async fn handle_final(
        &self,
        _metadata: &Metadata,
        _client: &Client,
        _block: NumberFor<Block>,
    ) -> Result<(), Self::Error>;

    async fn try_finalize(
        &self,
        client: &Client,
        block: NumberFor<Block>,
    ) -> Result<Option<NumberFor<Block>>, Self::Error>;

    fn last_finalized_block(&self) -> Result<NumberFor<Block>, Self::Error>;

    fn last_pending_block(&self) -> Result<NumberFor<Block>, Self::Error>;

    fn purge_pending(&self, block: NumberFor<Block>) -> Result<(), Self::Error>;
}

#[async_trait]
pub trait MinimalClient<Block: BlockT> {
    type Error: std::error::Error + Send + 'static;

    async fn finalized_head(&self) -> Result<NumberFor<Block>, Self::Error>;

    async fn import_notification_stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = BlockImportEvent<Block>> + Send>>;

    async fn finality_notification_stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = FinalityImportEvent<Block>> + Send>>;

    async fn hash(
        &self,
        block_number: NumberFor<Block>,
    ) -> Result<Option<Block::Hash>, Self::Error>;

    async fn runtime_version_at(
        &self,
        block_id: Option<BlockId<Block>>,
    ) -> Result<RuntimeVersion, Self::Error>;

    async fn metadata_at(&self, block_id: Option<BlockId<Block>>) -> Result<Metadata, Self::Error>;

    async fn storage(
        &self,
        block_id: BlockId<Block>,
        key: &StorageKey,
    ) -> Result<Option<StorageData>, Self::Error>;
}

pub trait Decoder<T> {
    type Error: std::error::Error + Send + 'static;

    fn decode_events(
        &self,
        metadata: &Metadata,
        input: &mut &[u8],
    ) -> Result<Vec<(Phase, T)>, Self::Error>;
}

#[async_trait]
pub trait EventExtractor<Block: BlockT, Client, R, E> {
    type Error: std::error::Error + Send + 'static;

    async fn extract(
        client: &Client,
        block: NumberFor<Block>,
        phase: &Phase,
        event: &R,
    ) -> Result<E, Self::Error>;
}

pub trait IndexStore<R, E> {
    type Error: std::error::Error + Send + 'static;

    fn process_pending(
        &self,
        block: u32,
        hash: Vec<u8>,
        records: Vec<(Phase, R, E)>,
    ) -> Result<(), Self::Error>;

    fn process_finalized(
        &self,
        block: u32,
        block_hash: &[u8],
        records: Vec<(Phase, R, E)>,
    ) -> Result<(), Self::Error>;

    fn finalize(&self, block: u32) -> Result<(), Self::Error>;

    fn last_finalized_block(&self) -> Result<u32, Self::Error>;

    fn last_pending_block(&self) -> Result<u32, Self::Error>;

    fn non_finalized_blocks(&self) -> Result<Vec<(u32, Option<Vec<u8>>)>, Self::Error>;

    fn purge_pending(&self, block: u32) -> Result<(), Self::Error>;
}
