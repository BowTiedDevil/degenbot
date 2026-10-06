//! The scripted actor contracts for the wave-3 aave-side capture scenarios
//! (the aave sibling of the `V3CaptureHarness` precedent): hand-assembled
//! EVM bytecode deployed onto the [`ScratchDriver`](crate::capture::ScratchDriver)
//! chain, whose real `LOG` opcodes and real call frames manufacture the
//! aave event stream the updater replays.
//!
//! # Why hand-assembled bytecode (not a Solidity artifact)
//!
//! The wave-2 pool scenarios drive the REAL canonical `UniswapV3Pool` because
//! the capture must execute real pool bytecode (mint/burn/swap callbacks,
//! verification getters). The aave-side scenarios' load-bearing surface is
//! different: the *event grammar* (the exact topics/data shapes the decoders
//! and the ops parser consume) and the *RPC answers* (`getDiscountPercent`,
//! `DEBT_TOKEN_REVISION`) — not deep contract execution. Hand-assembled
//! runtime keeps the generator node-free, deterministic, and artifact-free
//! (no Solidity toolchain, no tier-3 artifact pin), while still executing as
//! REAL EVM frames: the logs are real `LOG` opcodes of real deployed code and
//! the served `eth_call` answers are real execution returns.
//!
//! # The two builder shapes
//!
//! - [`scripted_actor_creation_code`]: an *actor* whose runtime dispatches on
//!   the CALLDATA SIZE. A branch whose length matches emits the branch's log
//!   burst (real `LOGn` opcodes) and returns the branch's 32-byte word; any
//!   other calldata (e.g. a real `getDiscountPercent(address)` selector call)
//!   falls through to the actor's fallback word — the designed answer
//!   surface, not a silent catch-all (the fallback is a pinned scenario
//!   literal that the capture's replay gates consume). Length dispatch (not
//!   a calldata-byte load + shift) keeps the runtime to Frontier-era
//!   opcodes only: the capture's load-bearing surface is the event grammar
//!   and the RPC answers, not the fixture EVM's shift/byte lane, and the
//!   length lane is collision-free against the 36-byte selector calls by
//!   construction.
//! - [`scripted_coordinator_creation_code`]: a *coordinator* whose runtime
//!   sub-`CALL`s one or more actors with a zero-padded calldata of the
//!   branch's length (the fresh frame's memory is zero, so no stores are
//!   needed) and returns a word. One coordinator call frame is ONE
//!   transaction carrying several actors' logs — the paired-event shape the
//!   ops parser requires (a Pool `Repay` plus the vToken `Burn` must share a
//!   `transactionHash`).
//!
//! # Determinism
//!
//! Every byte is a pure function of the builder inputs — no RNG, no map
//! iteration, no toolchain reordering. The deployed addresses are a pure
//! function of the frame sequence (CREATE from the driver's sender), which
//! the generator pins as literals.

use alloy::primitives::{Address, B256};

/// One log a branch emits: exact topics (1..=4) + exact data bytes (the
/// empty-data `Upgraded` shape is legitimate — one indexed implementation
/// topic, zero data words).
#[derive(Clone, Debug)]
pub struct ScriptedLog {
    /// The log's topics, topic0 first.
    pub topics: Vec<B256>,
    /// The log's data payload (the ABI non-indexed fields, word-packed).
    pub data: Vec<u8>,
}

impl ScriptedLog {
    /// A new scripted log.
    #[must_use]
    pub fn new(topics: Vec<B256>, data: Vec<u8>) -> Self {
        Self { topics, data }
    }
}

/// One dispatch branch of a scripted actor: when the call's calldata length
/// equals `calldata_len`, the runtime emits `logs` (in order) and returns
/// `return_word`.
#[derive(Clone, Debug)]
pub struct ActorBranch {
    /// The calldata length this branch answers (distinct per branch; the
    /// builders reject duplicates).
    pub calldata_len: u8,
    /// The log burst the branch emits (real `LOG` opcodes, in order).
    pub logs: Vec<ScriptedLog>,
    /// The 32-byte word the branch returns (the call's `eth_call` answer).
    pub return_word: [u8; 32],
}

impl ActorBranch {
    /// A new actor branch.
    #[must_use]
    pub fn new(calldata_len: u8, logs: Vec<ScriptedLog>, return_word: [u8; 32]) -> Self {
        Self {
            calldata_len,
            logs,
            return_word,
        }
    }
}

const PUSH1: u8 = 0x60;
const PUSH2: u8 = 0x61;
const PUSH4: u8 = 0x63;
const PUSH20: u8 = 0x73;
const PUSH32: u8 = 0x7f;
const DUP1: u8 = 0x80;
const EQ: u8 = 0x14;
const JUMPI: u8 = 0x57;
const JUMPDEST: u8 = 0x5b;
const POP: u8 = 0x50;
const CALLDATASIZE: u8 = 0x36;
const CODECOPY: u8 = 0x39;
const LOG0: u8 = 0xa0;
const MSTORE: u8 = 0x52;
const CALL: u8 = 0xf1;
const RETURN: u8 = 0xf3;

/// One memory page per log (every scripted data blob is padded to this page,
/// so the per-log `CODECOPY` regions stay disjoint by construction).
const LOG_MEMORY_PAGE: usize = 256;
/// Gas each coordinator sub-call grants its actor (LOG bursts are cheap).
const SUBCALL_GAS: u32 = 1_000_000;
/// The creation-code header width: `PUSH2 len; PUSH2 src; PUSH1 0; CODECOPY;
/// PUSH2 len; PUSH2 0; RETURN` (uniform two-byte immediates so any runtime
/// size up to 64 KiB assembles identically).
const CREATION_HEADER_LEN: usize = 3 + 3 + 2 + 1 + 3 + 3 + 1;

fn push1(buf: &mut Vec<u8>, value: u8) {
    buf.push(PUSH1);
    buf.push(value);
}

/// `PUSH2` immediate (big-endian) — the uniform encoding for code/blob
/// offsets and the creation length. The caller validates the bound (the
/// public builders turn an overflow into their `Result` error; the layout
/// doc's 16-bit ceiling is that check).
fn push2(buf: &mut Vec<u8>, value: u16) {
    buf.push(PUSH2);
    buf.extend_from_slice(&value.to_be_bytes());
}

fn push4(buf: &mut Vec<u8>, value: u32) {
    buf.push(PUSH4);
    buf.extend_from_slice(&value.to_be_bytes());
}

fn push20(buf: &mut Vec<u8>, value: &Address) {
    buf.push(PUSH20);
    buf.extend_from_slice(value.as_slice());
}

fn push32(buf: &mut Vec<u8>, value: &[u8; 32]) {
    buf.push(PUSH32);
    buf.extend_from_slice(value);
}

/// The runtime's return-word epilogue: `memory[0..32] = word; RETURN(0, 32)`.
/// `MSTORE` takes the offset on the stack top, so the value pushes deepest.
fn emit_return_word(buf: &mut Vec<u8>, word: &[u8; 32]) {
    push32(buf, word);
    push1(buf, 0x00);
    buf.push(MSTORE);
    push1(buf, 0x20);
    push1(buf, 0x00);
    buf.push(RETURN);
}

/// The epilogue's exact size (the layout pass needs body sizes).
const RETURN_WORD_EPILOGUE_LEN: usize = 2 + 33 + 1 + 2 + 2 + 1;

/// One branch's body length: `JUMPDEST; POP`, the per-log emission code, then
/// the return-word epilogue. The data blobs ride after ALL code. Per log the
/// `CODECOPY` carries its own size/src/dest words, and the `LOGn` carries its
/// OWN size/offset words on top of the topics (the two instructions' stack
/// shapes are independent — sharing the words underflows at the `LOGn`; the
/// offset is the uniform two-byte immediate since the second log's page base
/// 256 exceeds the one-byte bound).
fn branch_code_len(logs: &[ScriptedLog]) -> usize {
    let mut len = 2; // JUMPDEST; POP
    for log in logs {
        len += log.topics.len() * 33; // `PUSH32` per topic (pushed reversed)
        len += 2 + 3 + 3 + 1; // PUSH1 size; PUSH2 src; PUSH2 dest; CODECOPY
        len += 2 + 3 + 1; // PUSH1 size; PUSH2 offset; LOGn
    }
    len + RETURN_WORD_EPILOGUE_LEN
}

/// Validate one scripted log (the strictness the `log_emitter_initcode`
/// precedent sets: a generator that silently degrades its emitter is a bug).
/// The EMPTY-data log is legitimate (the `Upgraded` shape: one indexed
/// implementation topic, zero data words) — the `LOGn` size-0 read is exact.
fn validate_log(log: &ScriptedLog, context: &str) -> Result<(), String> {
    if log.topics.is_empty() || log.topics.len() > 4 {
        return Err(format!(
            "{context}: a scripted log needs 1..=4 topics, got {}",
            log.topics.len()
        ));
    }
    if log.data.len() > 255 {
        return Err(format!(
            "{context}: scripted log data {} bytes exceeds the 255 PUSH1 bound",
            log.data.len()
        ));
    }
    Ok(())
}

/// Assemble the actor runtime: a dispatch head (CALLDATASIZE on the stack),
/// one length test per branch, the fallback epilogue, then each branch's
/// body (`JUMPDEST; POP; <log bursts>; <return word>`), then every branch's
/// data blobs (each padded to one 256-byte memory page, sourced by the
/// `CODECOPY` the body emitted).
fn assemble_actor_runtime(
    branches: &[ActorBranch],
    fallback_word: &[u8; 32],
) -> Result<Vec<u8>, String> {
    let mut seen_lengths = Vec::new();
    for branch in branches {
        if seen_lengths.contains(&branch.calldata_len) {
            return Err(format!(
                "scripted actor: duplicate calldata length {}",
                branch.calldata_len
            ));
        }
        seen_lengths.push(branch.calldata_len);
        for log in &branch.logs {
            validate_log(log, "scripted actor branch")?;
        }
    }

    // Layout pass: JUMPDEST body starts, then the blob base (all code first).
    let head_len = 1; // CALLDATASIZE
    let length_test_len = 1 + 2 + 1 + 3 + 1; // DUP1; PUSH1 k; EQ; PUSH2 dest; JUMPI
    let fallback_len = 1 + RETURN_WORD_EPILOGUE_LEN; // POP; epilogue
    let mut body_starts = Vec::with_capacity(branches.len());
    let mut cursor = head_len + branches.len() * length_test_len + fallback_len;
    for branch in branches {
        body_starts.push(cursor);
        cursor += branch_code_len(&branch.logs);
    }
    let blob_base = cursor;

    let mut code = Vec::new();
    // Dispatch head: the calldata length on the stack.
    code.push(CALLDATASIZE);
    // Length tests (the size stays under the test result for the body POP).
    for (index, branch) in branches.iter().enumerate() {
        code.push(DUP1);
        push1(&mut code, branch.calldata_len);
        code.push(EQ);
        push2(
            &mut code,
            u16::try_from(body_starts[index])
                .map_err(|_| "branch body offset exceeds the 16-bit layout")?,
        );
        code.push(JUMPI);
    }
    // No branch matched: the fallback word — the actor's designed RPC answer
    // surface (e.g. the `getDiscountPercent` constant), never a revert.
    code.push(POP);
    emit_return_word(&mut code, fallback_word);
    // Branch bodies.
    for (index, branch) in branches.iter().enumerate() {
        debug_assert_eq!(code.len(), body_starts[index]);
        code.push(JUMPDEST);
        code.push(POP);
        for (log_index, log) in branch.logs.iter().enumerate() {
            // `LOGn` reads topic1 from the stack slot just below the size
            // word (the topics fill downward from there), so the LAST topic
            // pushes deepest: iterate REVERSED, ending on topic0 at the
            // `size` boundary. A forward loop silently reverses every
            // multi-topic log — the single-topic tests cannot see it and the
            // served topics come out backwards.
            for topic in log.topics.iter().rev() {
                push32(&mut code, topic.as_ref());
            }
            // CODECOPY(dest = log's memory page, src = its blob, size = len):
            // the stack is size (deepest), src, dest (top).
            push1(
                &mut code,
                u8::try_from(log.data.len()).map_err(|_| "log data exceeds 255 bytes")?,
            );
            let blob_offset = blob_base
                + branches[..index]
                    .iter()
                    .map(|b| b.logs.len() * LOG_MEMORY_PAGE)
                    .sum::<usize>()
                + log_index * LOG_MEMORY_PAGE;
            push2(
                &mut code,
                u16::try_from(blob_offset)
                    .map_err(|_| "log blob offset exceeds the 16-bit layout")?,
            );
            push2(
                &mut code,
                u16::try_from(log_index * LOG_MEMORY_PAGE)
                    .map_err(|_| "log memory page exceeds the 16-bit layout")?,
            );
            code.push(CODECOPY);
            // `LOGn`'s OWN size + offset words on top of the topics: stack is
            // topicN..topic1 (deepest, the reversed push above), size,
            // offset (top). The offset is the uniform two-byte immediate —
            // the second log's page base (256) does not fit a `PUSH1`.
            push1(
                &mut code,
                u8::try_from(log.data.len()).map_err(|_| "log data exceeds 255 bytes")?,
            );
            push2(
                &mut code,
                u16::try_from(log_index * LOG_MEMORY_PAGE)
                    .map_err(|_| "log memory page exceeds the 16-bit layout")?,
            );
            code.push(LOG0 + u8::try_from(log.topics.len()).map_err(|_| "topic count")?);
        }
        emit_return_word(&mut code, &branch.return_word);
    }
    debug_assert_eq!(code.len(), blob_base);
    // The data blobs (each padded to its 256-byte memory page).
    for branch in branches {
        for log in &branch.logs {
            let mut blob = log.data.clone();
            blob.resize(LOG_MEMORY_PAGE, 0);
            code.extend_from_slice(&blob);
        }
    }
    Ok(code)
}

/// Wrap a runtime into creation code: `CODECOPY(runtime); RETURN(runtime)` —
/// the header width is [`CREATION_HEADER_LEN`] (two-byte immediates), so the
/// runtime copies from that fixed offset.
///
/// # Errors
///
/// Errors when the runtime exceeds the two-byte immediate bounds (the
/// scenarios' runtimes sit far below them; the layout doc's ceiling).
fn creation_code_for(runtime: &[u8]) -> Result<Vec<u8>, String> {
    let runtime_len = u16::try_from(runtime.len())
        .map_err(|_| "scripted runtime exceeds the 16-bit creation layout".to_string())?;
    let header_len = u16::try_from(CREATION_HEADER_LEN)
        .map_err(|_| "creation header width exceeds u16".to_string())?;
    let mut code = Vec::with_capacity(CREATION_HEADER_LEN + runtime.len());
    // CODECOPY(dest = 0, src = the header width, size = len): the stack is
    // size (deepest), src, dest (top).
    push2(&mut code, runtime_len);
    push2(&mut code, header_len);
    push1(&mut code, 0x00);
    code.push(CODECOPY);
    // RETURN(0, len).
    push2(&mut code, runtime_len);
    push2(&mut code, 0);
    code.push(RETURN);
    debug_assert_eq!(code.len(), CREATION_HEADER_LEN);
    code.extend_from_slice(runtime);
    Ok(code)
}

/// Build the creation code of a scripted actor (see the module docs).
///
/// # Errors
///
/// Errors on a duplicated branch length, a log outside the 1..=4-topic /
/// 0..=255-byte grammar, or a runtime past the 16-bit layout bounds (the
/// scenarios' runtimes sit far below them).
pub fn scripted_actor_creation_code(
    branches: &[ActorBranch],
    fallback_word: [u8; 32],
) -> Result<Vec<u8>, String> {
    let runtime = assemble_actor_runtime(branches, &fallback_word)?;
    creation_code_for(&runtime)
}

/// Build the creation code of a scripted coordinator: the runtime sub-`CALL`s
/// each `(actor, calldata_len)` pair with a zero-padded calldata of the
/// branch's length (the fresh frame's memory is zero — no stores needed),
/// then returns the constant word. One coordinator call frame is ONE
/// transaction carrying every sub-called actor's logs.
///
/// # Errors
///
/// Errors only on the internal layout bounds (the scenarios' coordinators
/// sit far below them).
pub fn scripted_coordinator_creation_code(
    calls: &[(Address, u8)],
    return_word: [u8; 32],
) -> Result<Vec<u8>, String> {
    let mut runtime = Vec::new();
    for (actor, calldata_len) in calls {
        // CALL's stack (top = gas, deepest = retSize): retSize, retOffset,
        // argsSize, argsOffset, value, address, gas — pushed in that order so
        // the gas word lands on top. POP the success flag — a failed sub-call
        // leaves its logs unemitted, and the generator's recorded-entry
        // assertions catch that loudly, so the flag is not silently swallowed
        // here either.
        push1(&mut runtime, 0x20); // retSize
        push1(&mut runtime, 0x00); // retOffset
        push1(&mut runtime, *calldata_len); // argsSize
        push1(&mut runtime, 0x00); // argsOffset
        push1(&mut runtime, 0x00); // value
        push20(&mut runtime, actor);
        push4(&mut runtime, SUBCALL_GAS);
        runtime.push(CALL);
        runtime.push(POP);
    }
    emit_return_word(&mut runtime, &return_word);
    creation_code_for(&runtime)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn word(low_byte: u8) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[31] = low_byte;
        w
    }

    /// One-branch actor layout: header, dispatch head, length test,
    /// fallback, branch body (JUMPDEST; POP; topic; CODECOPY; LOG1; epilogue),
    /// then the padded data blob.
    #[test]
    fn actor_creation_code_layout_is_exact() {
        let topic = B256::from([1u8; 32]);
        let branch = ActorBranch::new(
            1,
            vec![ScriptedLog::new(vec![topic], vec![0xab; 96])],
            word(0x11),
        );
        let code = scripted_actor_creation_code(&[branch], word(0x22)).unwrap();
        // Header: PUSH2 len; PUSH2 16; PUSH1 0; CODECOPY; PUSH2 len; PUSH2 0;
        // RETURN (16 bytes).
        assert_eq!(code[0], PUSH2);
        assert_eq!(
            u16::from_be_bytes([code[1], code[2]]),
            u16::try_from(code.len() - CREATION_HEADER_LEN).unwrap()
        );
        assert_eq!(code[3], PUSH2);
        assert_eq!(
            u16::from_be_bytes([code[4], code[5]]),
            u16::try_from(CREATION_HEADER_LEN).unwrap()
        );
        assert_eq!(&code[6..9], &[0x60, 0x00, CODECOPY]);
        assert_eq!(code[9], PUSH2);
        assert_eq!(code[12], PUSH2);
        assert_eq!(&code[13..15], &[0x00, 0x00]);
        assert_eq!(code[15], RETURN);
        // Runtime head: CALLDATASIZE.
        let rt = &code[CREATION_HEADER_LEN..];
        assert_eq!(&rt[..1], &[CALLDATASIZE]);
        // Length test: DUP1; PUSH1 1; EQ; PUSH2 dest; JUMPI — the dest is the
        // branch body's JUMPDEST (head 1 + test 8 + fallback 42 = 51).
        assert_eq!(&rt[1..9], &[DUP1, PUSH1, 0x01, EQ, PUSH2, 0x00, 51, JUMPI]);
        // Fallback: POP + return word 0x22.
        assert_eq!(rt[9], POP);
        assert_eq!(rt[10], PUSH32);
        assert_eq!(&rt[11..43], &word(0x22));
        assert_eq!(&rt[43..45], &[0x60, 0x00]);
        assert_eq!(rt[45], MSTORE);
        assert_eq!(&rt[46..51], &[PUSH1, 0x20, PUSH1, 0x00, RETURN]);
        // Branch body at 51: `JUMPDEST`; `POP`; `push32` topic; the `CODECOPY` block
        // sources the blob at the body end (= 51 + 91 = 142); the LOG1's own
        // size/offset words (the offset is the two-byte immediate); epilogue.
        let body = &rt[51..];
        assert_eq!(body[0], JUMPDEST);
        assert_eq!(body[1], POP);
        assert_eq!(body[2], PUSH32);
        assert_eq!(&body[3..35], topic.as_slice());
        assert_eq!(
            &body[35..44],
            &[PUSH1, 96, PUSH2, 0x00, 142, PUSH2, 0x00, 0x00, CODECOPY]
        );
        assert_eq!(&body[44..49], &[PUSH1, 96, PUSH2, 0x00, 0x00]);
        assert_eq!(body[49], LOG0 + 1);
        // The branch's return word closes the body (the 41-byte epilogue).
        assert_eq!(body[50], PUSH32);
        assert_eq!(&body[51..83], &word(0x11));
        assert_eq!(&body[83..85], &[0x60, 0x00]);
        assert_eq!(body[85], MSTORE);
        assert_eq!(&body[86..91], &[PUSH1, 0x20, PUSH1, 0x00, RETURN]);
        // The padded data blob rides at 142 (96 data bytes + 160 zero pad).
        assert_eq!(&rt[142..238], &[0xab; 96][..]);
        assert_eq!(&rt[238..rt.len()], &[0u8; 160][..]);
    }

    /// Two-branch actor: the second branch's body and blobs sit after the
    /// first's — the `CODECOPY` source offsets must not overlap page regions.
    #[test]
    fn actor_two_branch_layout_keeps_blobs_disjoint() {
        let topic = B256::from([1u8; 32]);
        let log1 = vec![ScriptedLog::new(vec![topic], vec![0xab; 32])];
        let log2 = vec![ScriptedLog::new(vec![topic], vec![0xcd; 32])];
        let code = scripted_actor_creation_code(
            &[
                ActorBranch::new(1, log1.clone(), word(1)),
                ActorBranch::new(2, log2.clone(), word(2)),
            ],
            word(3),
        )
        .unwrap();
        let rt = &code[CREATION_HEADER_LEN..];
        let head_and_tests = 1 + 2 * 8;
        let fallback = 42;
        let body1 = head_and_tests + fallback;
        let body2 = body1 + branch_code_len(&log1);
        let blob_base = body2 + branch_code_len(&log2);
        // Branch 1's CODECOPY sources blob_base (the PUSH2 operand sits two
        // bytes past its opcode); branch 2's sources blob_base + 256.
        assert_eq!(
            u16::from_be_bytes([rt[body1 + 38], rt[body1 + 39]]),
            u16::try_from(blob_base).unwrap()
        );
        assert_eq!(
            u16::from_be_bytes([rt[body2 + 38], rt[body2 + 39]]),
            u16::try_from(blob_base + 256).unwrap()
        );
        // The blobs carry each branch's data, disjoint pages.
        assert_eq!(&rt[blob_base..blob_base + 32], &[0xab; 32][..]);
        assert_eq!(&rt[blob_base + 256..blob_base + 288], &[0xcd; 32][..]);
    }

    #[test]
    fn actor_rejects_out_of_grammar_shapes() {
        let topic = B256::from([1u8; 32]);
        // Duplicate branch lengths.
        let dup = scripted_actor_creation_code(
            &[
                ActorBranch::new(1, vec![ScriptedLog::new(vec![topic], vec![0x00])], word(1)),
                ActorBranch::new(1, vec![ScriptedLog::new(vec![topic], vec![0x00])], word(2)),
            ],
            word(3),
        );
        assert!(dup.is_err());
        // No topics / five topics / oversized data (the empty-data Upgraded
        // shape is LEGITIMATE — asserted positively in the smoke test).
        assert!(scripted_actor_creation_code(
            &[ActorBranch::new(
                1,
                vec![ScriptedLog::new(vec![], vec![0x00])],
                word(1)
            )],
            word(2)
        )
        .is_err());
        assert!(scripted_actor_creation_code(
            &[ActorBranch::new(
                1,
                vec![ScriptedLog::new(vec![topic; 5], vec![0x00])],
                word(1)
            )],
            word(2)
        )
        .is_err());
        assert!(scripted_actor_creation_code(
            &[ActorBranch::new(
                1,
                vec![ScriptedLog::new(vec![topic], vec![0u8; 256])],
                word(1)
            )],
            word(2)
        )
        .is_err());
    }

    /// Execution-level smoke: deploy + call the actor on a real fixture EVM.
    /// The branch call (matching calldata length) emits the log and returns
    /// the branch word; the selector-shaped call (36 bytes, no branch length)
    /// falls through to the fallback word; the empty-data log shape emits.
    #[test]
    fn actor_executes_on_the_fixture_evm() {
        use crate::oracle::{self, Output, TxSpec, Verdict};
        use alloy::primitives::Bytes;

        let mut evm = oracle::new_fixture_evm();
        oracle::set_disable_nonce_check(&mut evm, true);
        oracle::set_code_size_limits(&mut evm, Some(usize::MAX));
        let topic = B256::from([1u8; 32]);
        let creation = scripted_actor_creation_code(
            &[
                // Branch 1: one 32-byte-data log.
                ActorBranch::new(
                    1,
                    vec![ScriptedLog::new(vec![topic], vec![0xab; 32])],
                    word(0x11),
                ),
                // Branch 2: the Upgraded shape — one topic, ZERO data bytes.
                ActorBranch::new(
                    2,
                    vec![ScriptedLog::new(vec![topic], Vec::new())],
                    word(0x12),
                ),
            ],
            word(0x22),
        )
        .unwrap();
        let Verdict::Accepted {
            output: Output::Create(_, Some(actor)),
            logs: deploy_logs,
        } = oracle::transact(
            &mut evm,
            TxSpec::Deploy {
                init_code: Bytes::from(creation),
                gas: 16_700_000,
            },
        )
        else {
            panic!("actor deploy must succeed");
        };
        assert!(deploy_logs.is_empty(), "the deploy emits nothing");

        // Branch call: the log + the branch word.
        let verdict = oracle::transact(
            &mut evm,
            TxSpec::Call {
                to: actor,
                data: Bytes::from(vec![0u8; 1]),
                gas: 16_700_000,
            },
        );
        let Verdict::Accepted { output, logs } = verdict else {
            panic!("branch call must succeed, got {verdict:?}");
        };
        assert_eq!(logs.len(), 1, "the branch emits its log");
        assert_eq!(logs[0].address, actor);
        assert_eq!(logs[0].topics(), &[topic]);
        assert_eq!(logs[0].data.data.as_ref(), &[0xab; 32]);
        let Output::Call(ret) = output else {
            panic!("branch call return");
        };
        assert_eq!(ret.as_ref(), &word(0x11));

        // The second branch: the empty-data log (the Upgraded shape).
        let Verdict::Accepted { output, logs } = oracle::transact(
            &mut evm,
            TxSpec::Call {
                to: actor,
                data: Bytes::from(vec![0u8; 2]),
                gas: 16_700_000,
            },
        ) else {
            panic!("empty-data branch call must succeed");
        };
        assert_eq!(logs.len(), 1, "the empty-data branch emits its log");
        assert_eq!(logs[0].data.data.as_ref(), &[] as &[u8]);
        let Output::Call(ret) = output else {
            panic!("empty-data branch return");
        };
        assert_eq!(ret.as_ref(), &word(0x12));

        // Selector-shaped call (36 bytes): no branch — the fallback word, no log.
        let Verdict::Accepted { output, logs } = oracle::transact(
            &mut evm,
            TxSpec::Call {
                to: actor,
                data: Bytes::from([vec![0x6c, 0x53, 0x27, 0x2b], vec![0u8; 32]].concat()),
                gas: 16_700_000,
            },
        ) else {
            panic!("fallback call must succeed");
        };
        assert!(logs.is_empty(), "the fallback emits nothing");
        let Output::Call(ret) = output else {
            panic!("fallback call return");
        };
        assert_eq!(ret.as_ref(), &word(0x22));
    }

    /// Execution-level smoke: the coordinator's sub-calls carry the branch
    /// lengths, both actors' logs ride the coordinator's frame (the paired
    /// one-tx-many-log shape the ops parser groups by).
    #[test]
    fn coordinator_executes_its_sub_calls() {
        use crate::oracle::{self, Output, TxSpec, Verdict};
        use alloy::primitives::Bytes;

        let mut evm = oracle::new_fixture_evm();
        oracle::set_disable_nonce_check(&mut evm, true);
        oracle::set_code_size_limits(&mut evm, Some(usize::MAX));
        let topic_a = B256::from([2u8; 32]);
        let topic_b = B256::from([3u8; 32]);
        let first_creation = scripted_actor_creation_code(
            &[ActorBranch::new(
                1,
                vec![ScriptedLog::new(vec![topic_a], vec![0xcd; 16])],
                word(0x44),
            )],
            word(0x55),
        )
        .unwrap();
        let second_creation = scripted_actor_creation_code(
            &[ActorBranch::new(
                1,
                vec![ScriptedLog::new(vec![topic_b], vec![0xce; 16])],
                word(0x45),
            )],
            word(0x56),
        )
        .unwrap();
        let mut deploy = |creation: Vec<u8>| {
            let Verdict::Accepted {
                output: Output::Create(_, Some(address)),
                ..
            } = oracle::transact(
                &mut evm,
                TxSpec::Deploy {
                    init_code: Bytes::from(creation),
                    gas: 16_700_000,
                },
            )
            else {
                panic!("actor deploy");
            };
            address
        };
        let actor_a = deploy(first_creation);
        let actor_b = deploy(second_creation);
        let coordinator_creation =
            scripted_coordinator_creation_code(&[(actor_a, 1), (actor_b, 1)], word(0x66)).unwrap();
        let coordinator = deploy(coordinator_creation);
        let verdict = oracle::transact(
            &mut evm,
            TxSpec::Call {
                to: coordinator,
                data: Bytes::new(),
                gas: 16_700_000,
            },
        );
        let Verdict::Accepted { output, logs } = verdict else {
            panic!("coordinator call must succeed, got {verdict:?}");
        };
        // BOTH sub-called actors' logs ride the coordinator's frame.
        assert_eq!(logs.len(), 2, "both sub-calls' logs join the tx");
        assert_eq!(logs[0].address, actor_a);
        assert_eq!(logs[1].address, actor_b);
        let Output::Call(ret) = output else {
            panic!("coordinator return");
        };
        assert_eq!(ret.as_ref(), &word(0x66));
    }

    /// Multi-topic + multi-log regression: the topic push order is REVERSED
    /// at the machine level (`LOGn` reads topic1 from the stack slot just
    /// below the size word), so a forward push loop silently reverses every
    /// multi-topic log — invisible to single-topic tests, and the served
    /// topics come out backwards. The served topics must be in GENERATOR
    /// order, and a two-log branch must source its second log from its own
    /// memory page.
    #[test]
    fn actor_multi_topic_log_topics_serve_in_generator_order() {
        use crate::oracle::{self, Output, TxSpec, Verdict};
        use alloy::primitives::Bytes;

        let mut evm = oracle::new_fixture_evm();
        oracle::set_disable_nonce_check(&mut evm, true);
        oracle::set_code_size_limits(&mut evm, Some(usize::MAX));
        let topics: Vec<B256> = (1u8..=4).map(|i| B256::from([i; 32])).collect();
        let creation = scripted_actor_creation_code(
            &[
                // Branch 1: the four-topic shape (the aave `Repay` grammar:
                // topic0 event signature + three indexed words).
                ActorBranch::new(
                    1,
                    vec![ScriptedLog::new(topics.clone(), vec![0xee; 64])],
                    word(0x11),
                ),
                // Branch 2: TWO logs — the second log's `LOGn` offset is its
                // own 256-byte memory page (a one-byte immediate cannot carry
                // it).
                ActorBranch::new(
                    2,
                    vec![
                        ScriptedLog::new(vec![topics[0]], vec![0x11; 32]),
                        ScriptedLog::new(vec![topics[1]], vec![0x22; 32]),
                    ],
                    word(0x12),
                ),
            ],
            word(0x22),
        )
        .unwrap();
        let Verdict::Accepted {
            output: Output::Create(_, Some(actor)),
            ..
        } = oracle::transact(
            &mut evm,
            TxSpec::Deploy {
                init_code: Bytes::from(creation),
                gas: 16_700_000,
            },
        )
        else {
            panic!("actor deploy must succeed");
        };

        // Branch 1: four topics, EXACT generator order.
        let Verdict::Accepted { logs, .. } = oracle::transact(
            &mut evm,
            TxSpec::Call {
                to: actor,
                data: Bytes::from(vec![0u8; 1]),
                gas: 16_700_000,
            },
        ) else {
            panic!("four-topic branch call must succeed");
        };
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].topics(), topics.as_slice(), "topic order");
        assert_eq!(logs[0].data.data.as_ref(), &[0xee; 64]);

        // Branch 2: two logs, each with its own topic and data intact.
        let Verdict::Accepted { logs, .. } = oracle::transact(
            &mut evm,
            TxSpec::Call {
                to: actor,
                data: Bytes::from(vec![0u8; 2]),
                gas: 16_700_000,
            },
        ) else {
            panic!("two-log branch call must succeed");
        };
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].topics(), &[topics[0]]);
        assert_eq!(logs[0].data.data.as_ref(), &[0x11; 32]);
        assert_eq!(logs[1].topics(), &[topics[1]]);
        assert_eq!(logs[1].data.data.as_ref(), &[0x22; 32]);
    }
}
