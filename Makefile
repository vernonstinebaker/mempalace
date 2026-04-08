BINARY := target/release/mempalace-mcp
INSTALL_PATH := $(HOME)/bin/mempalace-mcp

.PHONY: build install test clean

build:
	cargo build --release

# Use cp — ln -sf also works but cp is cleaner.
# Note: if OpenCode (or another client) already has the old binary running,
# kill that process first before invoking the new one, or just restart the client.
install: build
	cp "$(abspath $(BINARY))" "$(INSTALL_PATH)"
	@echo "Installed: $(INSTALL_PATH)"

test:
	# End-to-end test script to be implemented
	@echo "No test script configured - run benchmarks manually"

clean:
	cargo clean
