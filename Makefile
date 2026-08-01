.PHONY: preflight relay-test capture rust-test rust-fmt

preflight:
	@sudo ./scripts/preflight-server.sh

relay-test:
	@python3 -m unittest discover -s tests -v

capture:
	@test -n "$(HOST_IP)" || (echo 'usage: make capture HOST_IP=10.10.0.X' >&2; exit 2)
	@sudo ./scripts/capture-civ6.sh "$(HOST_IP)"

rust-test:
	@cargo test --workspace

rust-fmt:
	@cargo fmt --all -- --check
