.PHONY: check test build release
LEVEL ?= minor
check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked
test:
	cargo test --locked
build:
	HERDR_OMNI_BUILD_FROM_SOURCE=1 sh scripts/install.sh
# Validate, bump both manifests, commit, tag, and push.
release: check
	cargo run --bin release-tool -- release $(LEVEL)
