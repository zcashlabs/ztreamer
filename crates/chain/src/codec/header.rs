//! [`RecordHeader`]: the height and the hashes of a stored block record, read from its envelope.

use zakura_chain::block;

use super::{CodecError, CompactBlockRecord, StoredBlock, protobuf_envelope};

/// The height, the hash and the previous hash of a stored block record.
///
/// Its decode reads only the envelope of the record: the format version, the three fields and the
/// length of the payload. It does not decode the protobuf payload. Thus it costs the same for an
/// empty block and for a full one. Read it where a request needs only the hash of a block, for
/// example the completing block of a subtree root.
///
/// ```
/// # use zakura_chain::block;
/// # use ztreamer_chain::codec::{CompactBlockRecord, RecordHeader, StoredBlock, TreeSizes};
/// let record = CompactBlockRecord {
///     height: 7,
///     hash: block::Hash([7; 32]),
///     previous_hash: block::Hash([6; 32]),
///     time: 0,
///     transactions: Vec::new(),
///     end_tree_sizes: TreeSizes::default(),
/// };
/// let header = RecordHeader::decode(&record.encode().unwrap()).unwrap();
/// assert_eq!((header.height, header.hash), (7, block::Hash([7; 32])));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordHeader {
    /// The height of the block.
    pub height: u32,
    /// The hash of the block, in the byte order of the block record.
    pub hash: block::Hash,
    /// The hash of the block below it.
    pub previous_hash: block::Hash,
}

impl StoredBlock for RecordHeader {
    fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        let (height, hash, previous_hash, _) = protobuf_envelope(bytes)?;
        Ok(Self { height, hash, previous_hash })
    }

    fn from_record(record: &CompactBlockRecord) -> Self {
        Self { height: record.height, hash: record.hash, previous_hash: record.previous_hash }
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn hash(&self) -> block::Hash {
        self.hash
    }

    fn previous_hash(&self) -> block::Hash {
        self.previous_hash
    }
}
