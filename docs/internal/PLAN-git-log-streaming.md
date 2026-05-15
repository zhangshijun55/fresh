# Plan: streaming `git show` into a file-backed buffer

## Executive summary

| | Current state | Final design |
|---|---|---|
| **Pre-pass** | `git show --numstat` runs first (5.4 s on bun's rewrite commit) just to find files to exclude before the real fetch even starts | Removed. Oversized files are handled at render time via path heuristics + `--stat` line counts. |
| **Transport** | `git show` stdout is captured into a 43 MB `String` in Rust, then crossed into QuickJS as one giant JS string. | `git show` stdout is piped **directly into a temp file** by the host. Bytes never enter the JS runtime. New optional 4th arg: `spawnProcess(cmd, args, cwd, { stdoutTo })`. |
| **JS work per commit** | `output.split("\n")` (~1 M JS strings) → loop building ~1 M `TextPropertyEntry` objects → marshal across FFI as one `Vec<TextPropertyEntry>`. | None. Plugin spawns, opens the resulting file, ticks for growth, awaits exit code. ~30 LOC of JS. |
| **Buffer storage** | Virtual buffer: `delete_bytes(0, 43M)` + `insert(0, &43_MB_string)` + bulk-add ~500 k `Overlay` objects to `marker_list`. | Real file-backed buffer (`BufferData::Unloaded`), 1 MB chunked load on scroll, bounded RSS. Diff coloring comes from the diff syntax grammar, not per-line overlays. |
| **Time to first paint** | Blocks until git fully exits + entries are built + overlays placed. ~6.7 s for the rewrite commit. | Buffer opens immediately at 0 bytes; the file grows under it; first paint as soon as the first KB lands (< 100 ms). |
| **Cancellation** | None. `pendingDetailId` only discards the *response*; the spawned `git show` keeps running for the full 5+ s after the user has moved on. Held `j` leaks a trail of zombie git processes. | `handle.kill()` on selection change. Requires adding the same `oneshot + tokio::select!` kill plumbing that `SpawnHostProcess` already has, to plugin `spawnProcess`. |
| **Repeat visits** | Re-runs the full git pipeline every time. | Cache miss writes to `~/.cache/fresh/git-show/<sha>`; cache hit is just `openFile(path)`, no git at all (commits are immutable). LRU in-memory layer on top. |
| **Long-line stalls** | 674 KB minified lines in lock files re-wrap every PageDown (~800 ms each). | Out of scope for this plan, but the file-backed buffer makes a renderer-side `nowrap` flag for diff views a tiny separate change. |
| **Code shape** | Detail panel is a "virtual buffer" wired to a 1 M-entry array built in JS. Re-built from scratch on every selection. | Detail panel is a regular file-backed buffer. Plugin code shrinks; host gains two small primitives (`stdoutTo`, `refreshBufferFromDisk`). No architectural shift. |

Three stackable PRs (~315 LOC of host changes, all mechanical, no
core refactor):

1. `spawnProcess` extensions: `stdoutTo` + kill plumbing.
2. `refreshBufferFromDisk` + `openFile({ largeFile: true })` option.
3. git-log plugin rewire: drop numstat, drop entry-array build, open
   stream directly.

## Problem

Opening the "Rewrite Bun in Rust" commit (`23427dbc` in bun) takes ~6.7 s and
leaves the editor unresponsive during render. Measured on a debug build with
the in-tree git-log plugin:

| Stage | Cost |
|---|---|
| `git show --numstat --format= <hash>` (blocking pre-pass) | **5.4 s** |
| `git show --stat --patch <hash>` (after exclusions) | **1.3 s**, 1,031,788 lines, 43 MB stdout |
| `stdout` → single QuickJS string | one 43 MB UTF-8 allocation in QuickJS |
| `.split("\n")` + `buildDetailLineEntry` loop | ~1 M JS strings + ~1 M `TextPropertyEntry` objects |
| FFI marshal of the entry array | crosses JS↔Rust as one giant `Vec<TextPropertyEntry>` |
| `set_virtual_buffer_content` (`virtual_buffers.rs:425`) | rebuilds piece tree (`delete_bytes(0, 43M)` + `insert(0, &text)`) and bulk-adds ~500 k `Overlay`s |
| Soft-wrap on long lines (e.g. 674 KB minified CSS in bun.lock) | ~800 ms per PageDown during scroll |

All git/diff/render logic should remain in the plugin (`plugins/git_log.ts`,
`plugins/lib/git_history.ts`). The core changes are limited to **two small
host primitives** that the plugin then composes.

## Goal

After this change, visiting a commit in the git-log view:

1. Spawns `git show --patch <hash>` with stdout piped **directly into a file**
   (never crossing into QuickJS).
2. Opens that file **immediately**, while git is still writing, as a normal
   file-backed buffer (`BufferData::Unloaded`, 1 MB chunked load).
3. Periodically grows the buffer's known length as bytes arrive on disk.
4. Awaits the spawn handle in the background for final exit code / error
   reporting.

The 43 MB stdout never touches JS. The buffer never holds 1 M overlays.
Cancellation is `handle.kill()`. Cache hits are `if exists(path) openFile(path)`
with no git invocation at all.

## Host changes

### 1. New optional `options` parameter on `spawnProcess`

```ts
// crates/fresh-editor/plugins/lib/fresh.d.ts (line ~2493)
spawnProcess(
  command: string,
  args: string[],
  cwd?: string,
  options?: { stdoutTo?: string },        // NEW
): ProcessHandle<SpawnResult>;
```

Semantics when `options.stdoutTo` is set:
- Host opens the path with `O_CREAT | O_TRUNC | O_WRONLY` and pipes the child
  process's stdout straight into it (`tokio::io::copy(&mut stdout, &mut file)`
  on the spawner task).
- `SpawnResult.stdout` resolves to `""`. `stderr` and `exit_code` are
  populated as today.
- IO errors during the pipe surface as `exit_code = -1` with the error in
  `stderr` (same shape as existing spawn failure).
- If the spawner has no local filesystem (remote/agent backend), the call
  rejects with a clear error — initial implementation is local-only.

### 2. New `refreshBufferFromDisk` host API

```ts
// fresh.d.ts
refreshBufferFromDisk(bufferId: number): Promise<void>;
```

For a file-backed buffer (`BufferData::Unloaded`), this:

1. Re-stats `file_path` on disk.
2. If `new_size > old_size`, extends the buffer's total length by
   `new_size - old_size`. Already-loaded chunks are untouched; the new tail is
   left unloaded (loads lazily when scrolled to, same as any other unloaded
   region).
3. If `new_size < old_size`, **ignores the change** and logs a warning.
   Streaming append-only is the only supported pattern; truncation is treated
   as corruption.
4. If a full line-feed scan was previously completed, marks it stale (or
   rescans only the appended range; see "Open questions").
5. Notifies the view that the document length changed (re-render scrollbar,
   gutter padding, etc.).

This is **not** a full reload — it is an O(1) length bump plus, optionally,
a tail-region line scan.

## Plugin changes

In `plugins/lib/git_history.ts` (`fetchCommitShow`) and
`plugins/git_log.ts` (`on_log_select` → `fetchAndRenderDetail`):

```ts
async function openCommitDetail(hash: string, cwd: string): Promise<number> {
  const tempPath = `${cacheDir(cwd)}/${hash}`;

  // Cache hit: skip git entirely.
  if (await editor.fileExists(tempPath)) {
    return editor.openFile(tempPath);
  }

  // Cache miss: spawn with stdoutTo, do NOT await.
  const handle = editor.spawnProcess(
    "git", ["show", "--patch", hash], cwd,
    { stdoutTo: tempPath },
  );

  // Open immediately — file may be 0 bytes; that's fine.
  const bufferId = await editor.openFile(tempPath);

  // Tick the buffer as git writes. ~5 fps is plenty for a 1-2 s diff.
  const ticker = editor.setInterval(
    () => editor.refreshBufferFromDisk(bufferId),
    200,
  );

  // Background: await git, final catch-up refresh, error reporting.
  void (async () => {
    try {
      const result = await handle;
      editor.refreshBufferFromDisk(bufferId);
      if (result.exit_code !== 0) {
        editor.setStatus(`git: ${result.stderr || "exit " + result.exit_code}`);
      }
    } finally {
      editor.clearInterval(ticker);
    }
  })();

  return bufferId;
}
```

Other plugin-side changes:

- **Drop `--numstat` pre-pass entirely.** Render-time heuristics handle
  oversized files: detect by file path (lock files, `*.min.*`, `*-lock.json`)
  and/or by `--stat` line counts in the patch header. Initial version: just
  drop numstat; deal with long files via the wrap fix below.
- **Drop `buildCommitDetailEntries` for the diff body.** The detail panel
  is now a real buffer holding the raw `git show` text; use the existing diff
  syntax-highlight grammar instead of synthesizing per-line
  `TextPropertyEntry` + `Overlay` objects.
- **Cancellation**: store the latest `handle` per selection. On selection
  change, `previousHandle.kill()` (already supported via the existing
  `killHostProcess` IPC path).
- **LRU cache** of recently-visited `(hash → bufferId)` so back/forward
  navigation is instant; `tempPath` doubles as the on-disk cache layer
  (commits are immutable).

## Doability analysis (code-verified)

### What exists and is reusable

| Concept | Location | Status |
|---|---|---|
| `BufferData::Unloaded { file_path, file_offset, bytes }` | `model/piece_tree.rs:21` | ✅ Field-of-`usize`, trivial to grow |
| Lazy chunk load | `model/buffer/mod.rs:1369` (`chunk_split_and_load`, `LOAD_CHUNK_SIZE = 1 MB`) | ✅ Unchanged |
| Large-file open path with empty-file handling | `model/buffer/mod.rs:600` (`if file_size > 0 { PieceTree::new(...) } else { PieceTree::empty() }`) | ✅ Already handles 0-byte case |
| Newline count without loading | `model/filesystem.rs:442` (`count_line_feeds_in_range`) | ✅ For tail-only rescan |
| Read range from disk | `model/filesystem.rs:433` (`read_range`) | ✅ Direct kernel read, atomic snapshot |
| `editor.delay(ms)` for one-shot waits | `plugin_dispatch.rs:3383` | ✅ Ticker = `while (!done) { await editor.delay(200); refresh(); }` |
| `ProcessSpawner` trait for local/remote/docker | `services/remote/spawner.rs:54` | ✅ Trait already abstracts the backend |

### What needs to be built or modified

**Verified gaps (each is the size noted, no architectural blockers found):**

1. **`PluginCommand::SpawnProcess` variant** (`fresh-core/src/api.rs:1951`)
   currently carries `{command, args, cwd, callback_id}`. Add
   `stdout_to: Option<PathBuf>`. _~3 LOC._

2. **QuickJS binding** (`quickjs_backend.rs:4634`, `spawn_process_start`)
   accepts `(command, args, cwd)`. Add optional 4th `options` arg parsed via
   rquickjs; pass through to `PluginCommand::SpawnProcess`. _~15 LOC._

3. **`handle_spawn_process` in dispatch** (`plugin_dispatch.rs:3267`)
   calls `spawner.spawn(command, args, effective_cwd).await`. The
   `ProcessSpawner` trait method (`services/remote/spawner.rs:56`) only
   takes `(command, args, cwd)`. Two options:
   - Extend trait method to `spawn(command, args, cwd, stdout_to: Option<PathBuf>)`.
     Touches `LocalProcessSpawner`, `RemoteProcessSpawner`,
     `DockerExecSpawner`. _~30 LOC._
   - Add a new trait method `spawn_to_file(...)` with a default impl that
     buffers in memory and writes the file. Cleaner. _~20 LOC._

4. **`LocalProcessSpawner::spawn`** (`services/remote/spawner.rs:71`) today
   uses `cmd.output().await` which buffers stdout in memory. For
   `stdout_to`, switch to `cmd.stdout(Stdio::piped()).spawn()?` then
   `tokio::io::copy(&mut child.stdout, &mut file).await`. _~20 LOC._

5. **`RemoteProcessSpawner::spawn` + `DockerExecSpawner::spawn`** —
   return a clear error for `stdout_to.is_some()` until someone needs it.
   _~5 LOC each._

6. **⚠ Kill plumbing for plugin `spawnProcess` is missing today.**
   `handle_spawn_host_process` (`plugin_dispatch.rs:1789`) has the
   `host_process_handles` + `kill_rx` pattern — but
   `editor.spawnProcess` (`handle_spawn_process` at line 3267) is a
   different code path **without** kill plumbing. The `_killHostProcess`
   JS API only kills `SpawnHostProcess` calls, which are
   authority-bypassing internals (`devcontainer up`), not user plugins. To
   cancel `git show` mid-stream we either:
   - Add the same `oneshot::channel + tokio::select!` kill pattern to
     `handle_spawn_process`. _~40 LOC._
   - Or expose `SpawnHostProcess` to plugins for this case (probably wrong;
     it bypasses the authority).

   **Recommended**: add kill plumbing to `handle_spawn_process` as part
   of this work. Net effect: `editor.spawnProcess` returns a handle whose
   `.kill()` works for both `stdoutTo` and non-`stdoutTo` calls. The
   trait method becomes `spawn(...) -> impl Future + Cancellable`, or
   the dispatch wraps it with the same kill pattern as line 1822.

7. **Forcing large-file mode at open** (`model/buffer/mod.rs:406`):
   ```rust
   if file_size >= threshold {
       Self::load_large_file(...)
   } else {
       Self::load_small_file(...)
   }
   ```
   For a 0-byte temp file the small-file path is taken — eager `read_file`
   into a `Loaded` buffer, no future "extend from disk" possible. Need
   either:
   - An `openFile(path, { largeFile: true })` flag that bypasses the size
     check. _~15 LOC, touches `plugin_dispatch` open-file handler +
     `Buffer::load_from_file` signature._
   - Or have the plugin sleep ~50 ms after spawn before `openFile` so git
     emits a few KB. Brittle; not recommended.

   **Verified**: `load_large_file_internal` (`mod.rs:527`) already handles
   `file_size == 0` correctly at line 600, so forcing the unloaded path on
   a 0-byte file works as-is.

8. **`refreshBufferFromDisk` host API**. There is no existing lightweight
   reload primitive — only `revert_buffer_by_id`
   (`file_operations.rs:1307`) which is **too heavy**: it rebuilds the
   entire `EditorState` via `from_file_with_languages`, re-runs encoding
   detection, replaces cursors, etc. We need:
   - A new `TextBuffer::extend_from_disk()` method on
     `model/buffer/mod.rs` that re-stats `persistence.file_path()`,
     computes `delta = new_size - old_size`, and appends a new piece
     pointing at the file tail.
   - A new piece-tree primitive `piece_tree.append_unloaded(buffer_id,
     file_offset, bytes)` — mechanical, since the tree already supports
     multi-piece buffers via `chunk_split_and_load`.
   - Plus a new `PluginCommand::RefreshBufferFromDisk { buffer_id,
     callback_id }` variant, a `handle_refresh_buffer_from_disk` in
     `plugin_dispatch.rs`, and the JS binding. _~80 LOC total._

   **Buffer-internal mechanics**:
   - `TextBuffer.buffers: Vec<StringBuffer>` (`mod.rs` field) — the existing
     `StringBuffer` with `BufferData::Unloaded { bytes }`. Bump `bytes` to
     match the new file size (or, simpler, **add a new StringBuffer
     pointing at the appended range** and an `Added` piece referencing it).
   - The "add a new StringBuffer for the appended tail" approach is
     cleaner because it doesn't mutate existing pieces — already-loaded
     chunks for the prefix stay untouched.

9. **`editor.fileExists`** — verified absent from `fresh.d.ts`. Either add
   it (trivial, ~10 LOC) or have the plugin do `try { editor.openFile(p) }
   catch { spawn... }`. Prefer the explicit existence check.

### Verified non-issues

- **Empty-file open in large-file mode**: `load_large_file_internal` at
  `mod.rs:600` already branches on `file_size > 0`. ✅
- **Concurrent `read_range` during refresh**: `read_range` goes through
  `FileSystem::read_range` (`filesystem.rs:433`) which goes straight to the
  kernel via `pread` — each call sees an atomic snapshot. Only risk is
  short reads past EOF, which `chunk_split_and_load` should handle (verify
  in implementation; if it currently panics on short read, fix to retry).
- **No new file-watcher infrastructure needed**: `FileWatcherManager`
  exists (`services/file_watcher.rs:36`) but is **not used** here. JS
  polling is simpler for a 1–2 s spawn lifetime.

### Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| Kill plumbing for `spawnProcess` doesn't exist today | Medium | Add as part of this PR (item 6). Without it, zombie git processes accumulate when user holds `j`. |
| `chunk_split_and_load` may not tolerate "tail past current EOF" reads | Low | Audit the code at `mod.rs:1369`; if it panics on short read, fix to either retry on next refresh or return zero bytes. Likely already fine since file-backed mode assumes the file is fixed. |
| Threshold decision happens at open time | Low | Mitigated by `openFile({ largeFile: true })` option (item 7). |
| Heavy `revert_buffer_by_id` doesn't fit streaming | Verified | Adding lightweight `extend_from_disk` (item 8) is the answer; don't reuse revert. |
| Per-leaf line scan needs re-running after extension | Medium | Either defer scan until spawn finishes (simpler), or extend the incremental scan to resume from previous end-of-scan offset. |

## Open questions

1. **Temp file location**: under `std::env::temp_dir()` (no caching, cleaned
   on reboot) or `~/.cache/fresh/git-show/<repo-id>/<sha>` (persistent SHA
   cache, ~30 KB to a few MB per commit, manual eviction). **Default: cache
   dir** — commits are immutable, repeat visits are free.
2. **Large-file mode threshold at open time**: a 0-byte file currently takes
   the `load_small_file` path. We must either (a) special-case "expected to
   grow" via an `openFile` option, or (b) always open the SHA-cache directory
   in large-file mode. Simpler: have the plugin do `await
   editor.delay(50)` after spawn so git has time to produce at least a few
   KB before `openFile`. Brittle. **Preferred fix**: add an
   `openFile(path, { largeFile: true })` option that forces the unloaded path.
3. **Line-feed scan during growth**: full scan is offered after the file
   stops growing. Two options:
   - Defer the scan until the spawn handle resolves; offer it via the same
     "Scan file for exact line numbers? (y/N)" prompt the user sees today.
   - Incremental scan: each `refreshBufferFromDisk` scans only the new tail
     using `count_line_feeds_in_range(path, old_size, new_size - old_size)`
     and updates a running per-leaf counter. Cheaper UX but more code.
4. **Cancellation cleanup**: when the user moves selection mid-stream and
   `handle.kill()` fires, do we delete the partial file or keep it?
   **Keep it** is fine — the SHA-cache is rebuilt on next visit (commit is
   immutable so a partial = corrupt cache; overwriting on next spawn is the
   simplest policy).
5. **Race: `refreshBufferFromDisk` mid-`read_range`**: the file-backed
   reader fetches `read_range(path, offset, len)` which goes straight to
   the kernel — each read sees an atomic snapshot at that moment. Risk is
   asking for `len` bytes past current EOF and getting a short read. Fix:
   `FileSystem::read_range` should already clamp / error on short reads;
   verify the contract at `model/filesystem.rs:433` and adjust the chunk
   loader to tolerate "tail not yet available, retry on next refresh".
6. **Long-line wrap stall**: orthogonal to this plan but still wanted — a
   renderer-side `nowrap` flag for diff-grammar buffers (or a per-buffer
   option set by the plugin on open). Without this fix, `bun.lock`-style
   674 KB lines remain ~800 ms/PageDown.

## Minimal change set (rough LOC estimates, post-analysis)

| # | Area | Files | LOC |
|---|---|---|---|
| 1–5 | `stdoutTo` plumbing | `fresh-core/api.rs`, `quickjs_backend.rs`, `plugin_dispatch.rs`, `services/remote/spawner.rs` (+docker, +remote stubs) | ~75 |
| 6 | Kill plumbing for `spawnProcess` (new — gap discovered during analysis) | `plugin_dispatch.rs` (move pattern from line 1822 into `handle_spawn_process`) | ~40 |
| 7 | `openFile({ largeFile: true })` option | `fresh.d.ts`, `plugin_dispatch.rs` open-file handler, `model/buffer/mod.rs` (signature on `load_from_file`) | ~25 |
| 8 | `refreshBufferFromDisk` + `extend_from_disk` + `piece_tree.append_unloaded` | `model/piece_tree.rs`, `model/buffer/mod.rs`, `plugin_dispatch.rs`, `fresh-core/api.rs`, `fresh.d.ts` | ~100 |
| 9 | `editor.fileExists` (optional helper) | `plugin_dispatch.rs`, `fresh.d.ts` | ~15 |
| — | Plugin rewire | `plugins/git_log.ts`, `plugins/lib/git_history.ts` | ~60 net (drops `buildCommitDetailEntries` body-loop + numstat) |

**Total: ~315 LOC of host changes**, no architectural shift. The biggest
revisions to the original estimate are item 6 (kill plumbing was assumed
present but isn't) and item 8 (no existing lightweight reload primitive —
`revert_buffer_by_id` is too heavy).

## Verdict

**Doable as designed.** All required primitives either exist or are
mechanical extensions of existing code. The two surprises uncovered
during analysis (missing kill plumbing on plugin spawn; no lightweight
reload) are both small additions, not architectural blockers. The
plan ships in three stackable PRs:

1. **PR 1: `spawnProcess` extensions** — `stdoutTo` (items 1–5) +
   kill plumbing (item 6). Independently useful for any plugin that
   spawns long-running processes.
2. **PR 2: `refreshBufferFromDisk` + `largeFile` option** — items 7–8.
   Independently useful for any "tail-f"-style buffer.
3. **PR 3: git-log plugin rewire** — pure plugin code, no host changes.
   Drops numstat, drops entry-array build, opens stream directly.

## Out of scope (separate work)

- Disabling soft-wrap in diff views (renderer fix; ~10 LOC, big perceived
  win but unrelated to streaming).
- A diff syntax grammar that classifies hunks / `+` / `-` for highlighting
  (probably already exists via syntect; just needs to be auto-applied to
  buffers opened from the SHA cache).
- Real progress events from the spawner (not needed; JS polling is enough
  for a 1–2 s diff).
- Streaming for arbitrary `spawnProcess` callers (out-of-scope; this
  proposal only adds `stdoutTo`, not bidirectional streaming).
