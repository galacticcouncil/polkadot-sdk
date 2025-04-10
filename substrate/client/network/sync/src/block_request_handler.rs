// Copyright (C) Parity Technologies (UK) Ltd.
// This file is part of Substrate.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate. If not, see <https://www.gnu.org/licenses/>.

//! Helper for handling (i.e. answering) block requests from a remote peer via the
//! `crate::request_responses::RequestResponsesBehaviour`.

use crate::{
	block_relay_protocol::{BlockDownloader, BlockRelayParams, BlockResponseError, BlockServer},
	schema::v1::{
		block_request::FromBlock as FromBlockSchema, BlockRequest as BlockRequestSchema,
		BlockResponse as BlockResponseSchema, BlockResponse, Direction,
	},
	service::network::NetworkServiceHandle,
	LOG_TARGET,
};

use codec::{Decode, DecodeAll, Encode};
use futures::{channel::oneshot, stream::StreamExt};
use log::debug;
use prost::Message;
use schnellru::{ByLength, LruMap};

use sc_client_api::BlockBackend;
use sc_network::{
	config::ProtocolId,
	request_responses::{IfDisconnected, IncomingRequest, OutgoingResponse, RequestFailure},
	service::traits::RequestResponseConfig,
	types::ProtocolName,
	NetworkBackend, MAX_RESPONSE_SIZE,
};
use sc_network_common::sync::message::{BlockAttributes, BlockData, BlockRequest, FromBlock};
use sc_network_types::PeerId;
use sp_blockchain::HeaderBackend;
use sp_runtime::{
	generic::BlockId,
	traits::{Block as BlockT, Header, One, Zero},
};

use std::{
	cmp::min,
	hash::{Hash, Hasher},
	sync::Arc,
	time::Duration,
};

/// Maximum blocks per response.
pub(crate) const MAX_BLOCKS_IN_RESPONSE: usize = 128;

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_NUMBER_OF_SAME_REQUESTS_PER_PEER: usize = 2;

mod rep {
	use sc_network::ReputationChange as Rep;

	/// Reputation change when a peer sent us the same request multiple times.
	pub const SAME_REQUEST: Rep = Rep::new_fatal("Same block request multiple times");

	/// Reputation change when a peer sent us the same "small" request multiple times.
	pub const SAME_SMALL_REQUEST: Rep =
		Rep::new(-(1 << 10), "same small block request multiple times");
}

/// Generates a `RequestResponseProtocolConfig` for the block request protocol,
/// refusing incoming requests.
pub fn generate_protocol_config<
	Hash: AsRef<[u8]>,
	B: BlockT,
	N: NetworkBackend<B, <B as BlockT>::Hash>,
>(
	protocol_id: &ProtocolId,
	genesis_hash: Hash,
	fork_id: Option<&str>,
	inbound_queue: async_channel::Sender<IncomingRequest>,
) -> N::RequestResponseProtocolConfig {
	N::request_response_config(
		generate_protocol_name(genesis_hash, fork_id).into(),
		std::iter::once(generate_legacy_protocol_name(protocol_id).into()).collect(),
		1024 * 1024,
		MAX_RESPONSE_SIZE,
		Duration::from_secs(20),
		Some(inbound_queue),
	)
}

/// Generate the block protocol name from the genesis hash and fork id.
fn generate_protocol_name<Hash: AsRef<[u8]>>(genesis_hash: Hash, fork_id: Option<&str>) -> String {
	let genesis_hash = genesis_hash.as_ref();
	if let Some(fork_id) = fork_id {
		format!("/{}/{}/sync/2", array_bytes::bytes2hex("", genesis_hash), fork_id)
	} else {
		format!("/{}/sync/2", array_bytes::bytes2hex("", genesis_hash))
	}
}

/// Generate the legacy block protocol name from chain specific protocol identifier.
fn generate_legacy_protocol_name(protocol_id: &ProtocolId) -> String {
	format!("/{}/sync/2", protocol_id.as_ref())
}

/// The key of [`BlockRequestHandler::seen_requests`].
#[derive(Eq, PartialEq, Clone)]
struct SeenRequestsKey<B: BlockT> {
	peer: PeerId,
	from: BlockId<B>,
	max_blocks: usize,
	direction: Direction,
	attributes: BlockAttributes,
	support_multiple_justifications: bool,
}

#[allow(clippy::derived_hash_with_manual_eq)]
impl<B: BlockT> Hash for SeenRequestsKey<B> {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.peer.hash(state);
		self.max_blocks.hash(state);
		self.direction.hash(state);
		self.attributes.hash(state);
		self.support_multiple_justifications.hash(state);
		match self.from {
			BlockId::Hash(h) => h.hash(state),
			BlockId::Number(n) => n.hash(state),
		}
	}
}

/// The value of [`BlockRequestHandler::seen_requests`].
enum SeenRequestsValue {
	/// First time we have seen the request.
	First,
	/// We have fulfilled the request `n` times.
	Fulfilled(usize),
}

/// The full block server implementation of [`BlockServer`]. It handles
/// the incoming block requests from a remote peer.
pub struct BlockRequestHandler<B: BlockT, Client> {
	client: Arc<Client>,
	request_receiver: async_channel::Receiver<IncomingRequest>,
	/// Maps from request to number of times we have seen this request.
	///
	/// This is used to check if a peer is spamming us with the same request.
	seen_requests: LruMap<SeenRequestsKey<B>, SeenRequestsValue>,
}

impl<B, Client> BlockRequestHandler<B, Client>
where
	B: BlockT,
	Client: HeaderBackend<B> + BlockBackend<B> + Send + Sync + 'static,
{
	/// Create a new [`BlockRequestHandler`].
	pub fn new<N: NetworkBackend<B, <B as BlockT>::Hash>>(
		network: NetworkServiceHandle,
		protocol_id: &ProtocolId,
		fork_id: Option<&str>,
		client: Arc<Client>,
		num_peer_hint: usize,
	) -> BlockRelayParams<B, N> {
		// Reserve enough request slots for one request per peer when we are at the maximum
		// number of peers.
		let capacity = std::cmp::max(num_peer_hint, 1);
		let (tx, request_receiver) = async_channel::bounded(capacity);

		let protocol_config = generate_protocol_config::<_, B, N>(
			protocol_id,
			client
				.block_hash(0u32.into())
				.ok()
				.flatten()
				.expect("Genesis block exists; qed"),
			fork_id,
			tx,
		);

		let capacity = ByLength::new(num_peer_hint.max(1) as u32 * 2);
		let seen_requests = LruMap::new(capacity);

		BlockRelayParams {
			server: Box::new(Self { client, request_receiver, seen_requests }),
			downloader: Arc::new(FullBlockDownloader::new(
				protocol_config.protocol_name().clone(),
				network,
			)),
			request_response_config: protocol_config,
		}
	}

	/// Run [`BlockRequestHandler`].
	async fn process_requests(&mut self) {
		while let Some(request) = self.request_receiver.next().await {
			let IncomingRequest { peer, payload, pending_response } = request;

			match self.handle_request(payload, pending_response, &peer) {
				Ok(()) => debug!(target: LOG_TARGET, "Handled block request from {}.", peer),
				Err(e) => debug!(
					target: LOG_TARGET,
					"Failed to handle block request from {}: {}", peer, e,
				),
			}
		}
	}

	fn handle_request(
		&mut self,
		payload: Vec<u8>,
		pending_response: oneshot::Sender<OutgoingResponse>,
		peer: &PeerId,
	) -> Result<(), HandleRequestError> {
		let request = crate::schema::v1::BlockRequest::decode(&payload[..])?;

		let from_block_id = match request.from_block.ok_or(HandleRequestError::MissingFromField)? {
			FromBlockSchema::Hash(ref h) => {
				let h = Decode::decode(&mut h.as_ref())?;
				BlockId::<B>::Hash(h)
			},
			FromBlockSchema::Number(ref n) => {
				let n = Decode::decode(&mut n.as_ref())?;
				BlockId::<B>::Number(n)
			},
		};

		let max_blocks = if request.max_blocks == 0 {
			MAX_BLOCKS_IN_RESPONSE
		} else {
			min(request.max_blocks as usize, MAX_BLOCKS_IN_RESPONSE)
		};

		let direction =
			i32::try_into(request.direction).map_err(|_| HandleRequestError::ParseDirection)?;

		let attributes = BlockAttributes::from_be_u32(request.fields)?;

		let support_multiple_justifications = request.support_multiple_justifications;

		let key = SeenRequestsKey {
			peer: *peer,
			max_blocks,
			direction,
			from: from_block_id,
			attributes,
			support_multiple_justifications,
		};

		let mut reputation_change = None;

		let small_request = attributes
			.difference(BlockAttributes::HEADER | BlockAttributes::JUSTIFICATION)
			.is_empty();

		match self.seen_requests.get(&key) {
			Some(SeenRequestsValue::First) => {},
			Some(SeenRequestsValue::Fulfilled(ref mut requests)) => {
				*requests = requests.saturating_add(1);

				if *requests > MAX_NUMBER_OF_SAME_REQUESTS_PER_PEER {
					reputation_change = Some(if small_request {
						rep::SAME_SMALL_REQUEST
					} else {
						rep::SAME_REQUEST
					});
				}
			},
			None => {
				self.seen_requests.insert(key.clone(), SeenRequestsValue::First);
			},
		}

		debug!(
			target: LOG_TARGET,
			"Handling block request from {peer}: Starting at `{from_block_id:?}` with \
			maximum blocks of `{max_blocks}`, reputation_change: `{reputation_change:?}`, \
			small_request `{small_request:?}`, direction `{direction:?}` and \
			attributes `{attributes:?}`.",
		);

		let maybe_block_response = if reputation_change.is_none() || small_request {
			let block_response = self.get_block_response(
				attributes,
				from_block_id,
				direction,
				max_blocks,
				support_multiple_justifications,
			)?;

			// If any of the blocks contains any data, we can consider it as successful request.
			if block_response
				.blocks
				.iter()
				.any(|b| !b.header.is_empty() || !b.body.is_empty() || b.is_empty_justification)
			{
				if let Some(value) = self.seen_requests.get(&key) {
					// If this is the first time we have processed this request, we need to change
					// it to `Fulfilled`.
					if let SeenRequestsValue::First = value {
						*value = SeenRequestsValue::Fulfilled(1);
					}
				}
			}

			Some(block_response)
		} else {
			None
		};

		debug!(
			target: LOG_TARGET,
			"Sending result of block request from {peer} starting at `{from_block_id:?}`: \
			blocks: {:?}, data: {:?}",
			maybe_block_response.as_ref().map(|res| res.blocks.len()),
			maybe_block_response.as_ref().map(|res| res.encoded_len()),
		);

		let result = if let Some(block_response) = maybe_block_response {
			let mut data = Vec::with_capacity(block_response.encoded_len());
			block_response.encode(&mut data)?;
			Ok(data)
		} else {
			Err(())
		};

		pending_response
			.send(OutgoingResponse {
				result,
				reputation_changes: reputation_change.into_iter().collect(),
				sent_feedback: None,
			})
			.map_err(|_| HandleRequestError::SendResponse)
	}

	fn get_block_response(
		&self,
		attributes: BlockAttributes,
		mut block_id: BlockId<B>,
		direction: Direction,
		max_blocks: usize,
		support_multiple_justifications: bool,
	) -> Result<BlockResponse, HandleRequestError> {
		let get_header = attributes.contains(BlockAttributes::HEADER);
		let get_body = attributes.contains(BlockAttributes::BODY);
		let get_indexed_body = attributes.contains(BlockAttributes::INDEXED_BODY);
		let get_justification = attributes.contains(BlockAttributes::JUSTIFICATION);

		let mut blocks = Vec::new();

		let mut total_size: usize = 0;

		let client_header_from_block_id =
			|block_id: BlockId<B>| -> Result<Option<B::Header>, HandleRequestError> {
				if let Some(hash) = self.client.block_hash_from_id(&block_id)? {
					return self.client.header(hash).map_err(Into::into)
				}
				Ok(None)
			};

		while let Some(header) = client_header_from_block_id(block_id).unwrap_or_default() {
			let number = *header.number();
			let hash = header.hash();
			let parent_hash = *header.parent_hash();
			let justifications =
				if get_justification { self.client.justifications(hash)? } else { None };

			let (justifications, justification, is_empty_justification) =
				if support_multiple_justifications {
					let justifications = match justifications {
						Some(v) => v.encode(),
						None => Vec::new(),
					};
					(justifications, Vec::new(), false)
				} else {
					// For now we keep compatibility by selecting precisely the GRANDPA one, and not
					// just the first one. When sending we could have just taken the first one,
					// since we don't expect there to be any other kind currently, but when
					// receiving we need to add the engine ID tag.
					// The ID tag is hardcoded here to avoid depending on the GRANDPA crate, and
					// will be removed once we remove the backwards compatibility.
					// See: https://github.com/paritytech/substrate/issues/8172
					let justification =
						justifications.and_then(|just| just.into_justification(*b"FRNK"));

					let is_empty_justification =
						justification.as_ref().map(|j| j.is_empty()).unwrap_or(false);

					let justification = justification.unwrap_or_default();

					(Vec::new(), justification, is_empty_justification)
				};

			let body = if get_body {
				match self.client.block_body(hash)? {
					Some(mut extrinsics) =>
						extrinsics.iter_mut().map(|extrinsic| extrinsic.encode()).collect(),
					None => {
						log::trace!(target: LOG_TARGET, "Missing data for block request.");
						break
					},
				}
			} else {
				Vec::new()
			};

			let indexed_body = if get_indexed_body {
				match self.client.block_indexed_body(hash)? {
					Some(transactions) => transactions,
					None => {
						log::trace!(
							target: LOG_TARGET,
							"Missing indexed block data for block request."
						);
						// If the indexed body is missing we still continue returning headers.
						// Ideally `None` should distinguish a missing body from the empty body,
						// but the current protobuf based protocol does not allow it.
						Vec::new()
					},
				}
			} else {
				Vec::new()
			};

			let block_data = crate::schema::v1::BlockData {
				hash: hash.encode(),
				header: if get_header { header.encode() } else { Vec::new() },
				body,
				receipt: Vec::new(),
				message_queue: Vec::new(),
				justification,
				is_empty_justification,
				justifications,
				indexed_body,
			};

			let new_total_size = total_size +
				block_data.body.iter().map(|ex| ex.len()).sum::<usize>() +
				block_data.indexed_body.iter().map(|ex| ex.len()).sum::<usize>();

			// Send at least one block, but make sure to not exceed the limit.
			if !blocks.is_empty() && new_total_size > MAX_BODY_BYTES {
				break
			}

			total_size = new_total_size;

			blocks.push(block_data);

			if blocks.len() >= max_blocks as usize {
				break
			}

			match direction {
				Direction::Ascending => block_id = BlockId::Number(number + One::one()),
				Direction::Descending => {
					if number.is_zero() {
						break
					}
					block_id = BlockId::Hash(parent_hash)
				},
			}
		}

		Ok(BlockResponse { blocks })
	}
}

#[async_trait::async_trait]
impl<B, Client> BlockServer<B> for BlockRequestHandler<B, Client>
where
	B: BlockT,
	Client: HeaderBackend<B> + BlockBackend<B> + Send + Sync + 'static,
{
	async fn run(&mut self) {
		self.process_requests().await;
	}
}

#[derive(Debug, thiserror::Error)]
enum HandleRequestError {
	#[error("Failed to decode request: {0}.")]
	DecodeProto(#[from] prost::DecodeError),
	#[error("Failed to encode response: {0}.")]
	EncodeProto(#[from] prost::EncodeError),
	#[error("Failed to decode block hash: {0}.")]
	DecodeScale(#[from] codec::Error),
	#[error("Missing `BlockRequest::from_block` field.")]
	MissingFromField,
	#[error("Failed to parse BlockRequest::direction.")]
	ParseDirection,
	#[error(transparent)]
	Client(#[from] sp_blockchain::Error),
	#[error("Failed to send response.")]
	SendResponse,
}

/// The full block downloader implementation of [`BlockDownloader].
#[derive(Debug)]
pub struct FullBlockDownloader {
	protocol_name: ProtocolName,
	network: NetworkServiceHandle,
}

impl FullBlockDownloader {
	fn new(protocol_name: ProtocolName, network: NetworkServiceHandle) -> Self {
		Self { protocol_name, network }
	}

	/// Extracts the blocks from the response schema.
	fn blocks_from_schema<B: BlockT>(
		&self,
		request: &BlockRequest<B>,
		response: BlockResponseSchema,
	) -> Result<Vec<BlockData<B>>, String> {
		response
			.blocks
			.into_iter()
			.map(|block_data| {
				Ok(BlockData::<B> {
					hash: Decode::decode(&mut block_data.hash.as_ref())?,
					header: if !block_data.header.is_empty() {
						Some(Decode::decode(&mut block_data.header.as_ref())?)
					} else {
						None
					},
					body: if request.fields.contains(BlockAttributes::BODY) {
						Some(
							block_data
								.body
								.iter()
								.map(|body| Decode::decode(&mut body.as_ref()))
								.collect::<Result<Vec<_>, _>>()?,
						)
					} else {
						None
					},
					indexed_body: if request.fields.contains(BlockAttributes::INDEXED_BODY) {
						Some(block_data.indexed_body)
					} else {
						None
					},
					receipt: if !block_data.receipt.is_empty() {
						Some(block_data.receipt)
					} else {
						None
					},
					message_queue: if !block_data.message_queue.is_empty() {
						Some(block_data.message_queue)
					} else {
						None
					},
					justification: if !block_data.justification.is_empty() {
						Some(block_data.justification)
					} else if block_data.is_empty_justification {
						Some(Vec::new())
					} else {
						None
					},
					justifications: if !block_data.justifications.is_empty() {
						Some(DecodeAll::decode_all(&mut block_data.justifications.as_ref())?)
					} else {
						None
					},
				})
			})
			.collect::<Result<_, _>>()
			.map_err(|error: codec::Error| error.to_string())
	}
}

#[async_trait::async_trait]
impl<B: BlockT> BlockDownloader<B> for FullBlockDownloader {
	fn protocol_name(&self) -> &ProtocolName {
		&self.protocol_name
	}

	async fn download_blocks(
		&self,
		who: PeerId,
		request: BlockRequest<B>,
	) -> Result<Result<(Vec<u8>, ProtocolName), RequestFailure>, oneshot::Canceled> {
		// Build the request protobuf.
		let bytes = BlockRequestSchema {
			fields: request.fields.to_be_u32(),
			from_block: match request.from {
				FromBlock::Hash(h) => Some(FromBlockSchema::Hash(h.encode())),
				FromBlock::Number(n) => Some(FromBlockSchema::Number(n.encode())),
			},
			direction: request.direction as i32,
			max_blocks: request.max.unwrap_or(0),
			support_multiple_justifications: true,
		}
		.encode_to_vec();

		let (tx, rx) = oneshot::channel();
		self.network.start_request(
			who,
			self.protocol_name.clone(),
			bytes,
			tx,
			IfDisconnected::ImmediateError,
		);
		rx.await
	}

	fn block_response_into_blocks(
		&self,
		request: &BlockRequest<B>,
		response: Vec<u8>,
	) -> Result<Vec<BlockData<B>>, BlockResponseError> {
		// Decode the response protobuf
		let response_schema = BlockResponseSchema::decode(response.as_slice())
			.map_err(|error| BlockResponseError::DecodeFailed(error.to_string()))?;

		// Extract the block data from the protobuf
		self.blocks_from_schema::<B>(request, response_schema)
			.map_err(|error| BlockResponseError::ExtractionFailed(error.to_string()))
	}
}
