/// Wrappers clients for use with the offchain workers
use crate::{
    metadata::{Error as MetadataError, Metadata},
    traits::{BlockImportEvent, FinalityImportEvent, MinimalClient},
};

use std::convert::TryInto;
use std::fmt;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use codec::Decode;
use futures::{Stream, StreamExt};

use sc_client_api::{backend::Backend as BackendT, client::BlockchainEvents, StorageProvider};
use sp_api::{CallApiAt, Metadata as MetadataT, ProvideRuntimeApi};

use sp_arithmetic::traits::AtLeast32Bit;
use sp_blockchain::{Error as BlockchainError, HeaderBackend, HeaderMetadata};
use sp_core::storage::StorageKey;
use sp_runtime::{
    generic::BlockId,
    traits::{Block as BlockT, Header as HeaderT, NumberFor},
};
use sp_storage::StorageData;
use sp_version::RuntimeVersion;

use frame_metadata::RuntimeMetadataPrefixed;

pub struct InProcess<Block, Client, Backend> {
    inner: Arc<Client>,
    _marker: PhantomData<(Block, Backend)>,
}

impl<Block: BlockT, Client, Backend> InProcess<Block, Client, Backend> {
    pub fn new(inner: Arc<Client>) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<Block: BlockT, Client, Backend> MinimalClient<Block> for InProcess<Block, Client, Backend>
where
    Client: ProvideRuntimeApi<Block>
        + BlockchainEvents<Block>
        + CallApiAt<Block>
        + HeaderBackend<Block>
        + HeaderMetadata<Block, Error = BlockchainError>
        + StorageProvider<Block, Backend>
        + Send
        + Sync
        + 'static,
    Client::Api: MetadataT<Block>,
    Backend: BackendT<Block> + 'static,
    <Block::Header as HeaderT>::Number: AtLeast32Bit,
    <<Block::Header as HeaderT>::Number as TryInto<u32>>::Error: fmt::Debug,
{
    type Error = BlockchainError;

    async fn finalized_head(&self) -> Result<NumberFor<Block>, Self::Error> {
        Ok(self.inner.info().finalized_number)
    }

    async fn import_notification_stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = BlockImportEvent<Block>> + Send>> {
        self.inner
            .import_notification_stream()
            .map(|b| BlockImportEvent { header: b.header })
            .boxed()
    }

    async fn finality_notification_stream(
        &self,
    ) -> Pin<Box<dyn Stream<Item = FinalityImportEvent<Block>> + Send>> {
        self.inner
            .finality_notification_stream()
            .map(|b| FinalityImportEvent { header: b.header })
            .boxed()
    }

    async fn hash(
        &self,
        block_number: NumberFor<Block>,
    ) -> Result<Option<Block::Hash>, Self::Error> {
        self.inner.hash(block_number)
    }

    async fn runtime_version_at(
        &self,
        block_id: Option<BlockId<Block>>,
    ) -> Result<RuntimeVersion, Self::Error> {
        let block = block_id.unwrap_or_else(|| BlockId::Hash(self.inner.info().finalized_hash));
        self.inner.runtime_version_at(&block).map_err(|e| e.into())
    }

    async fn metadata_at(&self, block_id: Option<BlockId<Block>>) -> Result<Metadata, Self::Error> {
        let block = block_id.unwrap_or_else(|| BlockId::Hash(self.inner.info().finalized_hash));
        let metadata = self.inner.runtime_api().metadata(&block)?;

        //#HACK simplest form of error handling for now
        //##HACK piggybacking on Error::Backend because Msg was removed
        RuntimeMetadataPrefixed::decode(&mut metadata.as_slice())
            .map_err(|e| BlockchainError::Backend(e.to_string()))?
            .try_into()
            .map_err(|e: MetadataError| BlockchainError::Backend(e.to_string()))
    }

    async fn storage(
        &self,
        block_id: BlockId<Block>,
        key: &StorageKey,
    ) -> Result<Option<StorageData>, Self::Error> {
        self.inner.storage(&block_id, key)
    }
}
