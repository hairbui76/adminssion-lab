# Installing Admission Lab

Admission Lab is a single binary named `admissionlab`. There are three
ways to get it, and this page is ordered by how most people should:

1. [**from a release**](#from-a-release) — a signed, checksummed archive
   for one of four platforms;
2. [**from source**](#from-source) — `cargo install --locked`, for a
   contributor or a platform with no published archive;
3. [**in CI**](#in-ci-the-github-action) — the composite action, which
   installs a pinned release for you and refuses to run an unverified
   one.

Whatever you install, `admissionlab` drives `docker`, `kind`, `kubectl`,
and `helm` as subprocesses and does not bundle them. Run
`admissionlab doctor` once after installing: it names every missing
prerequisite in one pass instead of failing halfway through a lab.

---

## Platforms

| Platform | Target | Archive |
| --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `admissionlab-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `aarch64-unknown-linux-gnu` | `admissionlab-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `admissionlab-<version>-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `admissionlab-<version>-x86_64-apple-darwin.tar.gz` |

Each release also publishes `SBOM.spdx.json`, `SHA256SUMS`, and the
signature pair `SHA256SUMS.sig` / `SHA256SUMS.pem`.

### What CI actually verified about each archive

`.github/workflows/release.yml` unpacks every archive **on the runner
that built it** and checks the layout, the executable bit, and the
binary's own architecture. It then runs the binary — `--version` must
equal the tag's version, and `doctor --help` and `test --help` must exit
0 — on the three targets whose runner shares their architecture.

The macOS Intel archive is cross-built on an Apple Silicon runner (this
is the one target Apple's own toolchain handles first-class). It is
executed under Rosetta 2 when the runner image provides it, and verified
structurally only when it does not; the release's build log says which
happened. Nothing here claims an Intel Mac was involved.

### Windows

There is **no native Windows build and no native Windows support**, and
this is a decision rather than a gap. `kind` and Docker behave
differently enough on native Windows that Admission Lab does not commit
to the behavior it reports there.

The supported Windows path is **WSL2**: install a WSL2 distribution,
install a Linux Docker daemon inside it (Docker Desktop's WSL2
integration or Docker Engine in the distribution itself), and use the
Linux x86_64 archive from inside WSL2. Admission Lab is then running on
Linux, which is a platform it does support — the Windows host is only
where the window is.

---

## From a release

### 1. Download

Take the archive for your platform, `SHA256SUMS`, and the two signature
files from the
[Releases page](https://github.com/hairbui76/admission-lab/releases).

```bash
version=1.0.0-rc.1
target=x86_64-unknown-linux-gnu
base="https://github.com/hairbui76/admission-lab/releases/download/v${version}"

curl --fail --location --remote-name "${base}/admissionlab-${version}-${target}.tar.gz"
curl --fail --location --remote-name "${base}/SHA256SUMS"
curl --fail --location --remote-name "${base}/SHA256SUMS.sig"
curl --fail --location --remote-name "${base}/SHA256SUMS.pem"
```

`SBOM.spdx.json` is worth downloading too if anything in your
organization consumes bills of materials; it is covered by the same
`SHA256SUMS`.

### 2. Verify the checksum

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

On macOS, `shasum --algorithm 256 --check --ignore-missing SHA256SUMS`.

`--ignore-missing` is what lets one `SHA256SUMS` verify the one archive
you downloaded rather than demanding all five artifacts. Read the
output: it must name your archive followed by `OK`. A run that prints
nothing but a warning about missing files has verified **nothing**.

### 3. Verify the signature

`SHA256SUMS` is signed with a keyless Sigstore certificate. There is no
Admission Lab signing key to look up, publish, or leak: the certificate
is issued against the release workflow's GitHub OIDC identity and
recorded in the public Rekor transparency log, so what you are checking
is *"this checksum file was produced by that workflow, in that
repository, at that tag"*.

```bash
cosign verify-blob \
  --certificate SHA256SUMS.pem \
  --signature SHA256SUMS.sig \
  --certificate-identity 'https://github.com/hairbui76/admission-lab/.github/workflows/release.yml@refs/tags/v1.0.0-rc.1' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  SHA256SUMS
```

The exact `--certificate-identity` for a given release is printed in
that release's notes — it carries the tag, so it differs per release.
Never substitute `--certificate-identity-regexp '.*'`: an identity that
matches anything turns this check into "somebody, somewhere, signed
something".

Because the signature covers `SHA256SUMS`, and `SHA256SUMS` covers every
archive and the SBOM, verifying those two files is enough. There is no
per-archive signature to check.

### 4. Unpack and put it on your PATH

Each archive holds one directory named after the archive, containing the
binary, `LICENSE`, and `README.md`:

```text
admissionlab-1.0.0-rc.1-x86_64-unknown-linux-gnu/
├── admissionlab
├── LICENSE
└── README.md
```

```bash
tar --extract --gzip --file "admissionlab-${version}-${target}.tar.gz"
install -D -m 0755 \
  "admissionlab-${version}-${target}/admissionlab" \
  "${HOME}/.local/bin/admissionlab"
```

Then make sure `~/.local/bin` is on your `PATH` (`/usr/local/bin` works
just as well and usually already is):

```bash
export PATH="${HOME}/.local/bin:${PATH}"   # add to your shell profile
admissionlab --version
```

On macOS the binary is unsigned and un-notarized, so Gatekeeper
quarantines anything downloaded through a browser and the first run dies
with "cannot be opened because the developer cannot be verified". Clear
the quarantine attribute **after** you have verified the checksum and
the signature above — those are the check, and this only tells macOS you
made it:

```bash
xattr -d com.apple.quarantine "${HOME}/.local/bin/admissionlab" 2>/dev/null || true
```

A download made with `curl` usually carries no quarantine attribute at
all, which is why the command tolerates its absence.

### 5. Check the prerequisites

```bash
admissionlab doctor
```

`doctor` reports on `docker`, `kind`, `kubectl`, and `helm` — every
missing or too-old tool at once, with what to do about each. Exit code
`0` means the host can run a lab.

---

## From source

Admission Lab builds with the exact toolchain pinned in
`rust-toolchain.toml`; `rustup` installs it automatically the first time
you run `cargo` in the repository.

```bash
git clone https://github.com/hairbui76/admission-lab.git
cd admission-lab
cargo install --path crates/admissionlab-cli --locked
```

Or without cloning:

```bash
cargo install --git https://github.com/hairbui76/admission-lab.git admissionlab-cli --locked
```

`--locked` is not optional advice. It builds from the `Cargo.lock` this
repository reviewed and released from; without it, `cargo` re-resolves
dependencies to whatever is newest today, and you get a binary nobody
tested and no way to say which one you have.

### Reproducing what a release publishes

`scripts/verify-release.sh` runs the buildable half of the release
workflow on your own machine, for your own platform, with no tag and no
signing identity:

```bash
./scripts/verify-release.sh          # stage in a temp directory
./scripts/verify-release.sh --keep   # keep it and print the path
```

It builds with `cargo build --locked --release`, packages the same
tarball layout, generates `SBOM.spdx.json` with the same pinned
generator the workflow uses (`cargo-sbom`, installed with `--locked` if
absent), writes and verifies a `SHA256SUMS` covering both, proves a
one-byte change to the archive is rejected by that checksum, then
unpacks the archive and runs the packaged binary (`--version` must match
the manifest; `doctor --help` and `test --help` must exit 0). It fails
loudly on the first problem, and it fails if its SBOM generator pin and
the workflow's have drifted apart.

It is a Phase 9 exit-gate command: run it before tagging a release.

---

## In CI: the GitHub Action

For a repository that wants Admission Lab in a pipeline, do not script
the download — use the composite action, which does all of the above and
refuses to skip any of it:

```yaml
- uses: OWNER/admission-lab/.github/actions/admissionlab@v1
  with:
    config: admissionlab.yaml
    version: "1.0.0-rc.1"
    sha256: "<the archive's line from SHA256SUMS>"
```

`version` without `sha256` is refused: the action never installs a
binary it cannot verify. Take the checksum from the release's
`SHA256SUMS` — the line for
`admissionlab-<version>-x86_64-unknown-linux-gnu.tar.gz`, since the
action runs on Linux x86_64 runners only — after checking that file's
signature once, by hand, as above.

Leaving `version` empty switches the action to building the checked-out
working tree instead, which is what this repository's own CI does and is
not what a downstream repository should do.

[`docs/github-action.md`](github-action.md) documents every input, the
artifacts the action uploads (including on a failing run), and its
exit-code behavior.

---

## Uninstalling

Delete the binary (`rm ~/.local/bin/admissionlab`, or
`cargo uninstall admissionlab-cli` for a `cargo install`). Admission Lab
writes nothing else outside its run workspace under `$TMPDIR` and
whatever `--report-dir` you asked for; a run that was interrupted before
cleanup may have left `adlab-*` clusters behind, and
`kind get clusters` will say so.
