//! The `zstd` decoder.
//!
//! Split out of `decompress.rs` so that the argument below sits beside the
//! code it justifies. Every item here is behind the `zstd` feature, and
//! the gate is on the `mod` declaration rather than on 5 items.
//!
//! # The `zstd` window, which is the one thing a server can make us
//! allocate
//!
//! RFC 8878 §3.1.1.1.2: *"To properly decode compressed data, a decoder
//! will need to allocate a buffer of at least Window_Size bytes"*, and
//! `Window_Size` is declared in the frame header by whoever compressed
//! it — up to 3.75 TB. The same section grants the defence and names the
//! number to use:
//!
//! > In order to protect decoders from unreasonable memory requirements,
//! > a decoder is allowed to reject a compressed frame that requests a
//! > memory size beyond the decoder's authorized range.
//! >
//! > For improved interoperability, it's recommended for decoders to
//! > support values of Window_Size up to 8 MB and for encoders not to
//! > generate frames requiring a Window_Size larger than 8 MB.
//!
//! [`ZSTD_MAX_WINDOW`] is that 8 MB, and it is the same answer Chrome
//! reached for `Content-Encoding: zstd` (`ZSTD_d_windowLogMax`, 8 MiB,
//! with its own net error code for the frames it turns away). `ruzstd`
//! defaults to **100 MB** — `DEFAULT_MAX_WINDOW_SIZE`, read rather than
//! assumed — so this is a narrowing of somebody else's default and not a
//! belt on a bare waist. The check is made in `ruzstd` *before* the
//! allocation (`FrameDecoderState::new` calls `check_window_size` and
//! only then `DecoderScratch::new(window_size)`), so a rejected frame
//! costs nothing.
//!
//! **[`crate::limit::Limited`] cannot stand in for this**, which is why the
//! number is here at all: it counts bytes yielded to the caller, and a
//! window is one allocation made before the first byte is yielded. Two
//! further facts, both measured by reading `ruzstd` 0.9:
//!
//! - The window is not always reserved up front. `DecoderScratch::new`
//!   builds an EMPTY ring buffer that grows with the data, so a first
//!   frame declaring 8 MB and carrying ten bytes allocates ten bytes'
//!   worth. `DecodeBuffer::reset` — the path a SECOND frame takes on the
//!   same decoder — does `buffer.reserve(window_size)` eagerly. So the
//!   up-front allocation exists and needs two frames to reach.
//! - While a frame is unfinished the decoder must RETAIN `Window_Size`
//!   decoded bytes to resolve back-references, so up to 8 MB of
//!   plaintext can be held before any of it is handed over, whatever
//!   [`crate::limit::Limited`] was set to. That is the cost of the coding, not
//!   of this wrapper.
//!

use bytes::Bytes;
/// The largest `Window_Size` a `zstd` frame may declare before this client
/// refuses it — RFC 8878 §3.1.1.1.2's recommended interoperability floor,
/// and the same number Chrome settled on for `Content-Encoding: zstd`.
///
/// It is the only bound that can be placed on this coding's memory, since
/// the window is allocated before a byte is yielded and
/// [`crate::limit::Limited`] counts bytes yielded. See the module doc.
const ZSTD_MAX_WINDOW: u64 = 8 * 1024 * 1024;

/// How many decoded bytes are taken out of the frame decoder per call.
///
/// Not a limit on anything a caller can see — [`ZstdStream::drive`] loops
/// until the decoder stops producing — only how often it copies. Same
/// role as [`BROTLI_BUFFER`], and heap-resident rather than a stack array
/// because everything here is reached from a `poll_frame`, where this
/// crate already has two future-size guards.
const ZSTD_CHUNK: usize = 16 * 1024;

/// The `zstd` coding, and the buffering `ruzstd` does not do.
///
/// The other three decoders are push-shaped upstream: hand them bytes and
/// they hand back whatever came out. `ruzstd` is pull-shaped — its
/// `StreamingDecoder` reads from a source and its `FrameDecoder` wants a
/// whole block at a time — so the mismatch is absorbed here, in the one
/// place that knows a frame off the wire is not a frame of the coding.
///
/// `FrameDecoder::decode_from_to` is the seam used, and its contract is
/// exactly what is needed: *"The source slice may contain only parts of a
/// frame but must contain at least one full block to make progress […]
/// if read == 0 then the source did not contain a full block"*. So
/// [`Self::pending`] holds whatever has not been consumed and never
/// exceeds one zstd block plus a header, because everything else is
/// consumed on the spot.
///
/// **Frames are plural, and that is not a nicety.** RFC 8878 §3.1: *"the
/// decompressed content of multiple concatenated frames is the
/// concatenation of each frame's decompressed content"*, and neither
/// `ruzstd` entry point streams across a frame boundary — `StreamingDecoder`
/// stops at `is_finished()`, `decode_from_to` never re-initialises. So
/// [`Self::start_frame`] does, and the alternative — stopping at the
/// first frame's end — would hand a caller a body silently missing
/// everything after it.
pub(crate) struct ZstdStream {
    dec: ruzstd::decoding::FrameDecoder,
    /// Compressed bytes that have arrived and are not yet consumed.
    pending: Vec<u8>,
    /// The decoded-byte scratch `decode_from_to` writes into.
    out: Vec<u8>,
}

impl ZstdStream {
    pub(super) fn new() -> Self {
        let mut dec = ruzstd::decoding::FrameDecoder::new();
        // The narrowing this crate makes over `ruzstd`'s own 100 MB
        // `DEFAULT_MAX_WINDOW_SIZE`. `ruzstd` checks it before it
        // allocates, so a refused frame costs nothing.
        dec.set_max_window_size(ZSTD_MAX_WINDOW);
        Self {
            dec,
            pending: Vec::new(),
            out: vec![0; ZSTD_CHUNK],
        }
    }

    pub(super) fn push(&mut self, input: &[u8]) -> Result<Bytes, std::io::Error> {
        self.pending.extend_from_slice(input);
        self.drive(false)
    }

    pub(super) fn finish(&mut self) -> Result<Bytes, std::io::Error> {
        let out = self.drive(true)?;
        // `drive` stops when it needs input it does not have. At the end
        // of the body there is none coming, so anything left over — a
        // half-written block, a frame whose last block never arrived, a
        // missing four-byte checksum — is a truncation rather than a
        // pause. Without this the bytes that DID arrive would decode
        // perfectly well and reach the caller as a shorter document,
        // which is the same defect gzip's trailer check exists against.
        if !self.dec.is_finished() || !self.pending.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the zstd body ended in the middle of a frame",
            ));
        }
        Ok(out)
    }

    /// Decodes everything the bytes in hand allow.
    ///
    /// `eof` says whether more compressed bytes may still arrive, and it
    /// is read in exactly one place: deciding whether too few bytes for a
    /// frame header mean *wait* or *this frame is truncated*.
    fn drive(&mut self, eof: bool) -> Result<Bytes, std::io::Error> {
        let mut decoded = Vec::new();
        loop {
            // `is_finished()` is true both before the first frame and
            // after each completed one, which is the same question here:
            // is there a frame in progress to feed?
            if self.dec.is_finished() {
                // Everything the completed frame is still holding, before
                // `start_frame` resets the buffer under it.
                loop {
                    let n = std::io::Read::read(&mut self.dec, &mut self.out)?;
                    if n == 0 {
                        break;
                    }
                    decoded.extend_from_slice(&self.out[..n]);
                }
                self.verify_checksum()?;
                if !self.start_frame(eof)? {
                    break;
                }
                continue;
            }
            let (read, written) = self
                .dec
                .decode_from_to(&self.pending, &mut self.out)
                .map_err(std::io::Error::other)?;
            decoded.extend_from_slice(&self.out[..written]);
            self.pending.drain(..read);
            if read == 0 && written == 0 {
                // Neither consumed nor produced: the rest of a block has
                // not arrived. `decode_from_to` says so in as many words.
                break;
            }
        }
        Ok(Bytes::from(decoded))
    }

    /// Starts the next frame, answering whether there was one to start.
    ///
    /// A skippable frame (RFC 8878 §3.1.2) is stepped over rather than
    /// refused: it is user metadata a decoder is required to ignore, and
    /// `ruzstd` reports it as an error from `init` because `init` has no
    /// other channel.
    fn start_frame(&mut self, eof: bool) -> Result<bool, std::io::Error> {
        use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
        loop {
            if self.pending.is_empty() {
                return Ok(false);
            }
            // A zstd frame header is at most 18 bytes — magic 4, frame
            // header descriptor 1, window descriptor 1, dictionary id 4,
            // frame content size 8. Below that `init` cannot tell "not
            // yet" from "malformed", and it reports both as the same
            // error, so waiting is the only way to keep the two apart.
            // Once the body has ended there is nothing to wait for and a
            // short header IS malformed, which is what `eof` decides.
            if !eof && self.pending.len() < ZSTD_MAX_FRAME_HEADER {
                return Ok(false);
            }
            let mut src: &[u8] = &self.pending;
            match self.dec.init(&mut src) {
                Ok(()) => {
                    let consumed = self.pending.len() - src.len();
                    self.pending.drain(..consumed);
                    return Ok(true);
                }
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => {
                    let consumed = self.pending.len() - src.len();
                    let skip = consumed.saturating_add(length as usize);
                    if self.pending.len() < skip {
                        if eof {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "the zstd body ended inside a skippable frame",
                            ));
                        }
                        return Ok(false);
                    }
                    self.pending.drain(..skip);
                }
                Err(e) => return Err(std::io::Error::other(e)),
            }
        }
    }

    /// Compares the frame's XXH64 content checksum with the one computed
    /// while decoding — **which `ruzstd` does not do**.
    ///
    /// Read rather than assumed: `FrameDecoder` exposes
    /// `get_checksum_from_data()` and `get_calculated_checksum()` and
    /// compares them nowhere, so a body corrupted in a way that leaves
    /// the block structure intact decodes without complaint. The check is
    /// optional in the format — `Content_Checksum_flag` — and both
    /// accessors answer `None` when it was not sent, which is why this is
    /// a comparison of two `Option`s and not an assertion that one
    /// exists.
    fn verify_checksum(&self) -> Result<(), std::io::Error> {
        let (Some(want), Some(got)) = (
            self.dec.get_checksum_from_data(),
            self.dec.get_calculated_checksum(),
        ) else {
            return Ok(());
        };
        if want == got {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the zstd frame's content checksum does not match the decoded bytes",
        ))
    }
}

/// The largest a zstd frame header can be: magic number 4, frame header
/// descriptor 1, window descriptor 1, dictionary id 4, frame content size
/// 8 (RFC 8878 §3.1.1.1).
const ZSTD_MAX_FRAME_HEADER: usize = 18;
