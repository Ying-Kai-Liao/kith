# Homebrew formula

`kith.rb` here is a template: its sha256 values are placeholders. On a `v*` tag the release workflow runs
`scripts/update-formula.sh <version> SHA256SUMS`, attaches the filled `kith.rb` to the GitHub Release, and
`scripts/update-tap.sh` commits it to the tap https://github.com/Ying-Kai-Liao/homebrew-kith as `Formula/kith.rb`.

The tap step needs the repo secret `HOMEBREW_TAP_TOKEN`: a fine-grained personal access token with access to
`Ying-Kai-Liao/homebrew-kith` only and the Contents permission set to read and write
(GitHub > Settings > Developer settings > Fine-grained tokens; then this repo's Settings > Secrets and variables >
Actions > New repository secret). Without it the workflow warns and the tap keeps its previous formula; then copy the
release's `kith.rb` to `Formula/kith.rb` in the tap by hand, or run
`scripts/update-tap.sh <version> kith.rb` with your own `gh` login.

To fill it locally: `scripts/update-formula.sh 1.2.3 SHA256SUMS > kith.rb`.
