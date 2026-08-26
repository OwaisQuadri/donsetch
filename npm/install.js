#!/usr/bin/env node
'use strict';

// donsetch postinstall: download the prebuilt binary for this platform
// from GitHub Releases, verify SHA256 against a source-controlled pinned
// hash, and extract to ./binaries/.
//
// The binary is NOT bundled in the npm package — it's fetched at
// install time from the GitHub release matching this package version.
// This keeps the npm registry clean (no 35 MB binary tarballs) and
// uses the same release artifacts that manual users download.
//
// Hardening: tarball SHA256 is verified ONLY against PINNED_SHA256 below.
// A checksum file hosted alongside the tarball on the same GitHub release
// is NOT trusted for authorization (same origin, no independent audit) and
// is therefore never fetched or used. If no pinned hash exists for the
// current platform the installer fails closed before any download and
// instructs the user to build from source. To enable prebuilt installs for
// a future release, add independently audited SHA256 entries to
// PINNED_SHA256 (one per release asset).
//
// Supported platforms:
//   linux-x64      Linux x86_64 (glibc)
//   linux-arm64    Linux ARM64 (glibc)
//   darwin-arm64   macOS Apple Silicon
//   darwin-x64     macOS Intel
//   win32-x64      Windows x86_64

const https = require('https');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const REPO = 'dondai44423/donsetch';
const VERSION = require('./package.json').version;
const TAG = `v${VERSION}`;

// ── source-controlled pinned hashes ─────────────────────────────
// Map of platform key -> hex SHA256 of the release tarball asset.
// This repository currently contains no independently audited npm
// release hashes, so the map is intentionally empty. The installer
// fails closed when the current platform has no entry (see top-level
// check below, before any download or cached-binary reuse). To enable
// prebuilt installs, add audited entries:
//
//   const PINNED_SHA256 = {
//     'linux-x64':    '<64-hex-sha256-of-donsetch-linux-x64.tar.gz>',
//     'linux-arm64':  '<64-hex-sha256-of-donsetch-linux-arm64.tar.gz>',
//     'darwin-arm64': '<64-hex-sha256-of-donsetch-darwin-arm64.tar.gz>',
//     'darwin-x64':   '<64-hex-sha256-of-donsetch-darwin-x64.tar.gz>',
//     'win32-x64':    '<64-hex-sha256-of-donsetch-win32-x64.tar.gz>',
//   };
//
// Do not invent or reuse a checksum downloaded from the GitHub release
// itself — only add hashes that have been independently audited.
const PINNED_SHA256 = {};

// ── platform mapping ────────────────────────────────────────────
const PLATFORMS = {
  'linux-x64':    { asset: 'donsetch-linux-x64.tar.gz',    binary: 'donsetch'     },
  'linux-arm64':  { asset: 'donsetch-linux-arm64.tar.gz',  binary: 'donsetch'     },
  'darwin-arm64': { asset: 'donsetch-darwin-arm64.tar.gz', binary: 'donsetch'     },
  'darwin-x64':   { asset: 'donsetch-darwin-x64.tar.gz',   binary: 'donsetch'     },
  'win32-x64':    { asset: 'donsetch-win32-x64.tar.gz',    binary: 'donsetch.exe' },
};

const platKey = `${process.platform}-${process.arch}`;
const plat = PLATFORMS[platKey];

if (!plat) {
  const known = {
    'darwin-x64': 'prebuilt binaries exist — update donsetch to a version that ships one',
    'win32-arm64': 'no prebuilt binary yet — build from source (see below)',
  }[platKey];
  console.error(`donsetch: unsupported platform ${platKey}${known ? ` (${known})` : ''}`);
  console.error('');
  console.error('Supported platforms:');
  console.error('  linux-x64      Linux x86_64 (glibc)');
  console.error('  linux-arm64    Linux ARM64 (glibc)');
  console.error('  darwin-arm64   macOS Apple Silicon');
  console.error('  darwin-x64     macOS Intel');
  console.error('  win32-x64      Windows x86_64');
  console.error('');
  console.error('Build from source: https://github.com/' + REPO);
  process.exit(1);
}

// ── musl detection (Alpine etc.) ────────────────────────────────
// The Linux binaries are glibc-linked. On musl systems they install
// fine but can never exec (missing ld-linux loader) — fail HERE with
// the actual cause instead of a cryptic spawn error on first run.
//
// Checking for /lib/ld-musl-*.so.1 existence is a false positive on
// glibc systems that also have musl installed as a secondary libc
// (e.g. Arch/Manjaro with the `musl` package). Instead, check what
// the RUNNING Node process actually uses: read /proc/self/exe's ELF
// interpreter (.interp section). On glibc it's ld-linux-*.so.2, on
// musl it's ld-musl-*.so.1. An env var escape hatch is also provided.
if (process.platform === 'linux') {
  // Escape hatch: force glibc path even on a musl system (user knows
  // the binary will run, e.g. via gcompat or a glibc chroot).
  if (process.env.DONSETCH_FORCE_GLIBC === '1') {
    // skip musl check
  } else {
    let isMusl = false;
    try {
      // Read the ELF interpreter of /proc/self/exe (this Node process).
      // The .interp section starts at offset 0x18 in the ELF header
      // for the program header table, but the reliable way is to parse
      // the program headers for PT_INTERP.
      const exe = '/proc/self/exe';
      const fd = fs.openSync(exe, 'r');
      // ELF header: first 64 bytes for 64-bit.
      const hdr = Buffer.alloc(64);
      fs.readSync(fd, hdr, 0, 64, 0);
      // e_phoff at offset 0x20 (64-bit), e_phentsize at 0x36, e_phnum at 0x38.
      const e_phoff = hdr.readBigUInt64LE(0x20);
      const e_phentsize = hdr.readUInt16LE(0x36);
      const e_phnum = hdr.readUInt16LE(0x38);
      for (let i = 0; i < e_phnum; i++) {
        const ph = Buffer.alloc(e_phentsize);
        fs.readSync(fd, ph, 0, e_phentsize, Number(e_phoff) + i * e_phentsize);
        // PT_INTERP = 3
        if (ph.readUInt32LE(0) === 3) {
          // p_offset at 0x08 (64-bit), p_filesz at 0x20.
          const p_offset = Number(ph.readBigUInt64LE(0x08));
          const p_filesz = ph.readBigUInt64LE(0x20);
          const interp = Buffer.alloc(Number(p_filesz));
          fs.readSync(fd, interp, 0, Number(p_filesz), p_offset);
          fs.closeSync(fd);
          const loaderPath = interp.toString('ascii').replace(/\0/g, '').trim();
          isMusl = loaderPath.includes('ld-musl');
          break;
        }
      }
      if (!isMusl) fs.closeSync(fd);
    } catch (_) {
      // /proc/self/exe not readable (some containers). Fall back to
      // the old existence check, but only as a last resort.
      isMusl = fs.existsSync('/lib/ld-musl-x86_64.so.1')
        || fs.existsSync('/lib/ld-musl-aarch64.so.1');
    }
    if (isMusl) {
      console.error('donsetch: musl libc detected (Alpine?).');
      console.error('The prebuilt Linux binaries are glibc-linked and will not run.');
      console.error('');
      console.error('Options:');
      console.error('  - build from source: git clone https://github.com/' + REPO + ' && cargo build --release');
      console.error('  - use a glibc-based image/dist (debian, ubuntu, fedora)');
      console.error('  - set DONSETCH_FORCE_GLIBC=1 if you have a glibc compat layer (gcompat)');
      process.exit(1);
    }
  }
}

const binDir = path.join(__dirname, 'binaries');
const binaryPath = path.join(binDir, plat.binary);

// Fail closed before any network download — and before accepting an
// existing/cached binary — if no pinned hash exists for this platform. A
// checksum file hosted alongside the tarball on the same GitHub release
// is not trusted for authorization (same origin, no independent audit).
const expectedHash = PINNED_SHA256[platKey];
if (!expectedHash) {
  console.error(`donsetch: no pinned SHA256 hash for ${platKey} (${plat.asset}) in this version of the installer.`);
  console.error('The checksum file hosted alongside the release tarball on GitHub (e.g. *.sha256) is downloaded from the same origin as the tarball itself and is therefore not trusted for authorization.');
  console.error('Prebuilt binary download is disabled until an independently audited hash is pinned in npm/install.js (PINNED_SHA256).');
  console.error('');
  console.error('Build from source instead:');
  console.error(`  git clone https://github.com/${REPO}.git`);
  console.error('  cd donsetch && cargo build --release');
  console.error('');
  console.error('See npm/README.md and https://github.com/' + REPO + '#-install for details.');
  process.exit(1);
}

// Never trust an existing/cached binary without a binary-level
// provenance check. The pinned value above authenticates the release
// tarball, not an arbitrary file already present in this directory.
// Remove any existing binary so every successful install comes from a
// freshly downloaded and verified tarball. This also prevents a stale
// or attacker-replaced binary from being accepted on reinstall.
if (fs.existsSync(binaryPath)) {
  console.log(`donsetch: removing existing ${plat.binary} before verified reinstall`);
  try {
    fs.unlinkSync(binaryPath);
  } catch (err) {
    console.error(`donsetch: cannot remove existing ${binaryPath}: ${err.message}`);
    process.exit(1);
  }
}

fs.mkdirSync(binDir, { recursive: true });

const baseUrl = `https://github.com/${REPO}/releases/download/${TAG}`;
const assetUrl = `${baseUrl}/${plat.asset}`;

// ── download with redirect following (max 5 hops) ───────────────
function download(url, dest) {
  return new Promise((resolve, reject) => {
    const MAX_REDIRECTS = 5;
    function get(u, hops) {
      if (hops > MAX_REDIRECTS) {
        reject(new Error(`too many redirects for ${url}`));
        return;
      }
      const opts = { headers: { 'Accept': 'application/octet-stream', 'User-Agent': 'donsetch-npm-installer' } };
      https.get(u, opts, (res) => {
        if ([301, 302, 303, 307, 308].includes(res.statusCode)) {
          res.resume();
          const loc = res.headers.location;
          if (!loc) { reject(new Error(`redirect without Location from ${u}`)); return; }
          // Stay on https: never follow a downgrade to cleartext.
          if (loc.startsWith('http://')) {
            reject(new Error(`refusing http:// downgrade redirect to ${loc}`));
            return;
          }
          get(loc, hops + 1);
          return;
        }
        if (res.statusCode !== 200) {
          res.resume();
          reject(new Error(`HTTP ${res.statusCode} for ${u}`));
          return;
        }
        const file = fs.createWriteStream(dest);
        res.pipe(file);
        file.on('finish', () => { file.close(); resolve(); });
        file.on('error', (err) => {
          try { fs.unlinkSync(dest); } catch (_) {}
          reject(err);
        });
      }).on('error', reject);
    }
    get(url, 0);
  });
}

// ── main ────────────────────────────────────────────────────────
async function main() {
  const tarball = path.join(binDir, plat.asset);

  // 0. Windows: tar ships with Windows 10 1803+; older boxes lack it.
  //    Detect BEFORE downloading so the error names the real problem.
  if (process.platform === 'win32') {
    let hasTar = false;
    try { execFileSync('tar', ['--version'], { stdio: 'ignore' }); hasTar = true; } catch (_) {}
    if (!hasTar) {
      console.error('donsetch: `tar` not found on this Windows system.');
      console.error('tar ships with Windows 10 1803+. Update Windows, or extract manually');
      console.error('after downloading ' + assetUrl);
      process.exit(1);
    }
  }

  // 1. Download the binary tarball
  console.log(`donsetch: downloading ${plat.asset} from ${TAG}...`);
  await download(assetUrl, tarball);

  // 2. Verify SHA256 against the pinned hash (source-controlled)
  console.log('donsetch: verifying checksum...');
  const actualHash = crypto.createHash('sha256').update(fs.readFileSync(tarball)).digest('hex');

  if (actualHash !== expectedHash) {
    try { fs.unlinkSync(tarball); } catch (_) {}
    console.error(`donsetch: SHA256 mismatch!`);
    console.error(`  expected (pinned): ${expectedHash}`);
    console.error(`  actual:            ${actualHash}`);
    console.error('The download may have been corrupted or tampered with.');
    process.exit(1);
  }

  // 3. Extract (tar is built into Linux, macOS, and Windows 10+)
  console.log('donsetch: extracting...');
  // execFileSync: no shell, no string interpolation — the install
  // path (which can contain quotes/spaces) is passed as argv.
  execFileSync('tar', ['xzf', tarball, '-C', binDir], { stdio: 'inherit' });

  // 4. Cleanup
  try { fs.unlinkSync(tarball); } catch (_) {}

  // 5. Verify the binary exists after extraction (BEFORE chmod —
  //    chmod on a missing file throws an opaque error).
  if (!fs.existsSync(binaryPath)) {
    console.error(`donsetch: expected ${plat.binary} not found after extraction`);
    console.error(`  looked at: ${binaryPath}`);
    console.error('  contents of binaries/:', fs.readdirSync(binDir).join(', '));
    process.exit(1);
  }

  // 6. Make executable (Unix only)
  if (process.platform !== 'win32') {
    fs.chmodSync(binaryPath, 0o755);
  }

  console.log(`donsetch: installed ${plat.binary} to ${binaryPath}`);
  console.log(`donsetch: run \`donsetch\` to see available commands.`);
}

main().catch((err) => {
  console.error(`donsetch: install failed: ${err.message}`);
  console.error('');
  console.error('You can build from source:');
  console.error('  git clone https://github.com/' + REPO);
  console.error('  cd donsetch && cargo build --release');
  process.exit(1);
});
