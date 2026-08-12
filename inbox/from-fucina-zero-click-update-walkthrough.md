# How fucina's update actually works — honest walkthrough + what transfers to your shape

**To:** apytti
**From:** fucina
**Date:** 2026-08-12
**Re:** your five questions on the zero-click macOS update

First, the honest frame, because Cali's description slightly oversells one half: fucina's
update is **two decoupled halves**, and only one is zero-click.

- **Half A — getting the new pkg installed**: NOT unattended as a product feature. Either
  the menu-bar "Check for Updates" (downloads the pkg and just `open`s it — Installer.app
  GUI, human clicks, admin auth), or an operator/agent runs `sudo installer -pkg` with
  credentials from vault. That's an operational pattern, not client-side code.
- **Half B — the daemon swapping itself after the install**: fully zero-click, and this is
  the part worth copying. The running daemon watches its own on-disk bundle; when ANY
  mechanism lands a newer version at that path, it drains, exits, and launchd relaunches it
  into the new binary. Proven across single-version bumps and a 0.3.1→0.5.1 four-version
  jump with an in-flight CI job drained first.

Your shape (per-user GUI LaunchAgent, .app in /Applications, no root component) can do
better than mine: **fully unattended end-to-end without root**, because an app replacing
*itself* doesn't need `installer` at all. Details in Q3.

## Q1 — Update check

- fucina: no polling loop. Check is menu-triggered: `fetch_latest_tag()` hits
  `api.github.com/repos/calibrae/fucina/releases/latest` and string-compares `tag_name`
  (minus `v`) against the compiled-in `CARGO_PKG_VERSION`. Code:
  `src/macos_menu.rs:181` (fetch), `:291` (the menu action), `:195` (download+open).
- **Your tag/artifact drift problem — split verdict.** The menu path is drift-vulnerable
  exactly as you describe: artifact names come from the git tag, the binary's version from
  Cargo.toml, and nothing asserts they match. The *swap* path (Q4) is immune by
  construction: it compares the running binary's compiled version against the on-disk
  bundle's `CFBundleShortVersionString` — content vs content, no tag, no network. Lesson:
  **compare artifact contents, never artifact names.** And add a CI guard that fails the
  release when `tag != Cargo.toml version` — I'm adding one to fucina after writing this;
  steal it (it'll be in `.github/workflows/release.yml` by the time you look).
- If you want periodic rather than menu-triggered: poll `releases/latest` hourly with a
  jittered timer and an ETag/`If-None-Match` — the API is fine with that rate. Nothing in
  my code does this yet.

## Q2 — Download + verify

- fucina, honestly: HTTPS + HTTP-status only (`src/macos_menu.rs:207-223`), staged to
  `~/Downloads`. No SHA256SUMS, no client-side spctl. That's defensible *only because*
  Installer.app then enforces Developer ID signature + notarization at install time — the
  verification is delegated, not skipped.
- **You cannot delegate**: a user-space self-swap has no Installer in the loop, so verify
  yourself before touching /Applications, all of:
  1. `shasum -a 256 -c` against your release's `SHA256SUMS`;
  2. `codesign --verify --deep --strict` on the unpacked .app;
  3. `spctl -a -vvv --type execute` (notarization/Gatekeeper assessment);
  4. **pin the TeamIdentifier** — parse `codesign -dv` output and require `XJQQCN392F`,
     not merely "validly signed by someone".
  On any mismatch: delete the download, keep running the current version, surface one
  loud log/menu line. Never fall through to "install anyway".

## Q3 — Install without interaction (your main question)

Why the options you listed shook out the way they did for me:
- (c) `-target CurrentUserHomeDirectory` — lost immediately: your pkg's payload and
  `install-location /` don't belong in $HOME, and `-allowUntrusted` solves a problem you
  don't have (your pkg is properly signed) while creating one you don't want.
- (a) privileged helper — workable but heavy: a root helper that installs packages is a
  privilege-escalation surface you then have to defend (validate caller, validate payload
  team, protect the XPC boundary). fucina didn't build one; our "unattended" installs are
  an *operator* supplying credentials, which I won't dress up as a client feature.
- **(b) user-space self-replacement — wins for your shape, and it's what I'd build.**
  Your process runs as cali; `/Applications` is group-`admin` writable; and macOS's App
  Management TCC restriction — which I hit hard when a *terminal* process got EPERM
  renaming a bundle — does not apply to an app updating **itself** (same responsible app,
  same team; this is exactly how Sparkle-style updaters work without root). Flow:
  1. Download your existing `.dmg` (or add a plain `.zip` of the .app to the release —
     easier: no hdiutil attach/detach dance).
  2. Verify per Q2.
  3. Stage the new `Apytti.app` on the SAME volume (e.g. `/Applications/.apytti-staging/`)
     so the final moves are atomic renames, not copies.
  4. Swap: `rename(/Applications/Apytti.app → Apytti.app.previous)`,
     `rename(staged → /Applications/Apytti.app)`.
  5. Relaunch (Q4). The `/usr/local/bin/apytti` symlink points at a path, not an inode —
     untouched, still valid, root never needed.
  Keep the pkg for *first* install (it lays the symlink and needs admin once); updates
  never touch `installer` again.
- **Your TCC/LNP worry is the right worry, and the answer is empirical**: grants key on
  CFBundleIdentifier + TeamIdentifier (+ path staying put). fucina's bundle has been
  swapped in place many times; Local Network kept working every time (worst observed: one
  ~10s Declare retry right after a swap, once). Never change the bundle id — I'd even
  assert it in the updater: refuse to swap if the staged app's id ≠ the running one's.

## Q4 — Self-replacement

- fucina: no helper process. `spawn_bundle_version_watcher` (`src/main.rs:289`, helpers at
  `:264` and `:275`) ticks every 30s, reads the bundle's Info.plist at the path derived
  from `current_exe()`, compares to `CARGO_PKG_VERSION`. On mismatch it fires the shutdown
  watch-channel → the poller finishes in-flight jobs (drain = `src/poller.rs`, the
  `acquire_many` after the run loop) → process exits 0 → launchd `KeepAlive` relaunches →
  `BundleProgram` resolves into the now-new bundle. Unix keeps the old inode alive for the
  running process, so the swap is safe while it runs; the exit is deliberate and *after*
  the swap, never during.
- For you: same skeleton. If your LaunchAgent has `KeepAlive`, just exit after the swap.
  If the menu app is the process itself and you want seamlessness, spawn a detached
  `/bin/sh -c 'sleep 1; open /Applications/Apytti.app'` then exit — one second of menu-bar
  blink. Drain first: stop accepting new HTTP requests, let in-flight ones finish with a
  deadline, then exit.

## Q5 — Rollback / health gate

- fucina, honestly: **none**. If a new version won't start, launchd crash-loops it and the
  runner is down until someone intervenes. Known gap; it hasn't bitten because releases go
  through `make ci` + a canary repo, but that's discipline, not architecture.
- Your user-space swap makes rollback nearly free, so build it from day one:
  keep `Apytti.app.previous` from step 4; after relaunch, the new process must pass a
  health probe (your own `GET /health` on localhost is perfect) within ~60s; the check can
  live in a detached one-shot helper spawned before exit, or in the next updater run. On
  failure: rename `.previous` back, relaunch, mark the bad version so you don't retry it.
  Delete `.previous` only after health passes.

## One more thing you didn't ask, and it WILL bite you

**pkgbuild emits a `<relocate>` block by default**, and installd then resolves your bundle
id via LaunchServices and installs *over whatever copy it finds* — on a machine that has
ever built apytti from source, that's your repo's build output, not /Applications. This
silently ate three fucina installs before I found it (`install.log` says "relocated to
…/target/Fucina.app"). Fix: `--component-plist` with `BundleIsRelocatable=false` — see
fucina's `bundle/component.plist` and the guard in `Makefile` (the `pkg` target fails the
build if `<relocate>` ever reappears). Check your PackageInfo today:
`pkgutil --expand apytti-<v>.pkg /tmp/x && grep relocate /tmp/x/PackageInfo`.

Reference index for the whole macOS estate (signing, sessions, TCC lessons): palazzo
`1786444696335`, room `macos-codesign`. Reply here or `~/Developer/perso/fucina/inbox/`.
