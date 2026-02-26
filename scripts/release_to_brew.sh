#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Automate Homebrew formula generation for cowork-merge-rs.

Usage:
  scripts/release_to_brew.sh \
    --workspace <bitbucket-workspace> \
    --repo <bitbucket-repo-slug> \
    --version <0.1.0|v0.1.0> \
    --tap-dir <path-to-local-homebrew-tap>

Required args:
  --workspace   Bitbucket workspace name
  --repo        Bitbucket repository slug
  --version     Release version (with or without leading v)
  --tap-dir     Local path to tap repo root (must contain Formula/)

Optional args:
  --formula-name <name>   Formula/binary name (default: cowork-merge-rs)
  --license <id>          SPDX license string (default: MIT)
  --skip-tag              Do not create git tag locally
  --skip-push-tag         Do not push git tag to origin
  --skip-brew-test        Do not run brew install/test locally
  -h, --help              Show this help

Examples:
  scripts/release_to_brew.sh \
    --workspace myteam \
    --repo claude-cowork-sync \
    --version v0.2.0 \
    --tap-dir ~/git/homebrew-cowork

  scripts/release_to_brew.sh \
    --workspace myteam \
    --repo claude-cowork-sync \
    --version 0.2.0 \
    --tap-dir ~/git/homebrew-cowork \
    --skip-tag --skip-push-tag
USAGE
}

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Missing required command: $cmd" >&2
    exit 1
  fi
}

camel_case_formula_class() {
  local raw="$1"
  awk -F'-' '{
    out=""
    for (i = 1; i <= NF; i++) {
      part=$i
      first=toupper(substr(part,1,1))
      rest=substr(part,2)
      out=out first rest
    }
    print out
  }' <<<"$raw"
}

workspace=""
repo=""
version_raw=""
tap_dir=""
formula_name="cowork-merge-rs"
license_id="MIT"
skip_tag="false"
skip_push_tag="false"
skip_brew_test="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --workspace)
      workspace="${2:-}"
      shift 2
      ;;
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --version)
      version_raw="${2:-}"
      shift 2
      ;;
    --tap-dir)
      tap_dir="${2:-}"
      shift 2
      ;;
    --formula-name)
      formula_name="${2:-}"
      shift 2
      ;;
    --license)
      license_id="${2:-}"
      shift 2
      ;;
    --skip-tag)
      skip_tag="true"
      shift
      ;;
    --skip-push-tag)
      skip_push_tag="true"
      shift
      ;;
    --skip-brew-test)
      skip_brew_test="true"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$workspace" || -z "$repo" || -z "$version_raw" || -z "$tap_dir" ]]; then
  echo "Missing required arguments." >&2
  usage
  exit 1
fi

require_cmd git
require_cmd curl
require_cmd shasum
require_cmd awk

if [[ "$skip_brew_test" != "true" ]]; then
  require_cmd brew
fi

if [[ ! -d "$tap_dir" ]]; then
  echo "Tap directory does not exist: $tap_dir" >&2
  exit 1
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" ]]; then
  echo "Run this script inside the cowork-merge-rs git repository." >&2
  exit 1
fi
cd "$repo_root"

if [[ "$version_raw" == v* ]]; then
  tag="$version_raw"
else
  tag="v$version_raw"
fi

formula_class="$(camel_case_formula_class "$formula_name")"
formula_file="$tap_dir/Formula/$formula_name.rb"
homepage="https://bitbucket.org/$workspace/$repo"
tarball_url="https://bitbucket.org/$workspace/$repo/get/$tag.tar.gz"

echo "==> Release inputs"
echo "Repo root:   $repo_root"
echo "Tag:         $tag"
echo "Tarball URL: $tarball_url"
echo "Tap dir:     $tap_dir"
echo "Formula:     $formula_file"

if [[ "$skip_tag" != "true" ]]; then
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null 2>&1; then
    echo "==> Tag already exists locally: $tag"
  else
    echo "==> Creating local tag: $tag"
    git tag "$tag"
  fi
else
  echo "==> Skipping local tag creation"
fi

if [[ "$skip_push_tag" != "true" ]]; then
  echo "==> Pushing tag to origin: $tag"
  git push origin "$tag"
else
  echo "==> Skipping tag push"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

tarball_path="$tmp_dir/$formula_name-$tag.tar.gz"
echo "==> Downloading tarball"
curl -fL "$tarball_url" -o "$tarball_path"

sha256="$(shasum -a 256 "$tarball_path" | awk '{print $1}')"
echo "==> SHA256: $sha256"

mkdir -p "$tap_dir/Formula"

cat > "$formula_file" <<FORMULA
class $formula_class < Formula
  desc "Synchronize Claude Cowork state between two machines"
  homepage "$homepage"
  url "$tarball_url"
  sha256 "$sha256"
  license "$license_id"

  depends_on "rust" => :build
  depends_on "node"

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  def caveats
    <<~EOS
      Browser-state export/import requires Playwright Chromium:
        npx playwright@1.56.1 install chromium
    EOS
  end

  test do
    assert_match "$formula_name", shell_output("#{bin}/$formula_name --help")
  end
end
FORMULA

echo "==> Wrote formula: $formula_file"

if [[ "$skip_brew_test" != "true" ]]; then
  echo "==> Installing formula from local path"
  brew install --build-from-source "$formula_file"

  echo "==> Running brew test"
  brew test "$formula_name"
else
  echo "==> Skipping brew install/test"
fi

echo "==> Done"
echo "Next: commit and push tap repo changes from $tap_dir"
