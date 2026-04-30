BINARY := kubectl-sshuttle
GOBIN  ?= $(shell go env GOPATH)/bin

# rushtle (Rust) — optional component used when --rushtle is passed.
RUSHTLE_DIR     := rushtle
RUSHTLE_IMAGE   ?= xjjo/rushtle
RUSHTLE_TAG     ?= latest
RUSHTLE_REF     := $(RUSHTLE_IMAGE):$(RUSHTLE_TAG)
KIND_CLUSTER    ?= kind

# sshuttle (Python) prebaked image — replaces the legacy `apt-get + pip
# install at startup` flow with a non-root, immediately-ready container.
SSHUTTLE_DIR    := proxy
SSHUTTLE_IMAGE  ?= xjjo/sshuttle
SSHUTTLE_TAG    ?= latest
SSHUTTLE_REF    := $(SSHUTTLE_IMAGE):$(SSHUTTLE_TAG)

.PHONY: build test install clean rushtle rushtle-image rushtle-image-multiarch rushtle-push rushtle-test rushtle-kind-load rushtle-prebuilt release-snapshot krew-install-local krew-uninstall-local sshuttle-image sshuttle-push images-push

build:
	go build -o $(BINARY) .

test:
	go test ./... -v

install: build
	install -m 755 $(BINARY) $(GOBIN)/$(BINARY)

clean:
	rm -f $(BINARY)
	@$(MAKE) -C $(RUSHTLE_DIR) clean 2>/dev/null || true

# --- rushtle targets -------------------------------------------------------

# Build rushtle binary natively (debug build).
rushtle:
	cd $(RUSHTLE_DIR) && cargo build --release
	@echo "built: $(RUSHTLE_DIR)/target/release/rushtle"

# Build rushtle docker image. The Dockerfile builds a static musl binary
# inside the container so the host doesn't need musl-cross.
rushtle-image:
	docker build -t $(RUSHTLE_REF) $(RUSHTLE_DIR)

# Push the locally-built single-arch image to its registry.
rushtle-push: rushtle-image
	docker push $(RUSHTLE_REF)

# Build the prebaked sshuttle (Python) image. Same model as the rushtle
# image: sshuttle baked in, non-root user (uid=1001), no apt/pip at
# startup. Pod runs immediately.
sshuttle-image:
	docker build -t $(SSHUTTLE_REF) $(SSHUTTLE_DIR)

sshuttle-push: sshuttle-image
	docker push $(SSHUTTLE_REF)

# Convenience: build + push BOTH images.
images-push: rushtle-push sshuttle-push

# Build + push multi-arch image (linux/amd64 + linux/arm64) using buildx.
# Requires: a configured buildx builder. First time:
#   docker buildx create --use --name rushtle-builder
rushtle-image-multiarch:
	docker buildx build \
	    --platform linux/amd64,linux/arm64 \
	    -t $(RUSHTLE_REF) \
	    --push \
	    $(RUSHTLE_DIR)

# Load image into a local kind cluster for testing.
rushtle-kind-load: rushtle-image
	kind load docker-image $(RUSHTLE_REF) --name $(KIND_CLUSTER)

# Cross-build static rushtle binaries for the goreleaser/krew tarballs.
# Linux: docker buildx (QEMU on non-native hosts).
# Darwin: native cargo when host is macOS; skipped otherwise.
rushtle-prebuilt:
	bash scripts/build-rushtle-prebuilt.sh

# Local goreleaser dry-run (snapshot tag, no upload). Useful for sanity
# checking the .goreleaser.yml + .krew.yaml wiring before tagging.
release-snapshot: rushtle-prebuilt
	goreleaser release --snapshot --clean --skip=publish,sign

# Detect host OS/ARCH for krew local install. krew uses os=linux/darwin and
# arch=amd64/arm64 in its labels.
KREW_OS    := $(shell uname -s | tr A-Z a-z)
KREW_ARCH  := $(shell uname -m | sed -e 's/x86_64/amd64/' -e 's/aarch64/arm64/')

# Path to the snapshot tarball for the host OS/ARCH. Goreleaser names them
# kubectl-sshuttle_<version>_<os>_<arch>.tar.gz; we pick whichever exists.
KREW_TARBALL := $(firstword $(wildcard dist/kubectl-sshuttle_*_$(KREW_OS)_$(KREW_ARCH).tar.gz))

# Install the just-built snapshot tarball via krew, the same way users will
# get it from the krew-index. Use this to dry-run the full plugin install
# before tagging a release.
#
#   make release-snapshot       # build dist/*.tar.gz
#   make krew-install-local     # install host arch via krew
#   kubectl sshuttle --rushtle ...
#
# `kubectl krew remove sshuttle` to undo (or `make krew-uninstall-local`).
krew-install-local: release-snapshot
	@if [ -z "$(KREW_TARBALL)" ]; then \
	  echo "no snapshot tarball for $(KREW_OS)/$(KREW_ARCH); release-snapshot didn't produce one" >&2; \
	  exit 1; \
	fi
	@echo "==> installing $(KREW_TARBALL) via krew (host=$(KREW_OS)/$(KREW_ARCH))"
	@SHA=$$(sha256sum $(KREW_TARBALL) | awk '{print $$1}'); \
	  TMPDIR=$$(mktemp -d); \
	  printf 'apiVersion: krew.googlecontainertools.github.com/v1alpha2\nkind: Plugin\nmetadata:\n  name: sshuttle\nspec:\n  version: v0.0.0-local\n  homepage: https://github.com/jjo/kubectl-sshuttle\n  shortDescription: Tunnel traffic through a cluster (local snapshot)\n  description: Local snapshot install for testing\n  platforms:\n    - selector:\n        matchLabels:\n          os: "%s"\n          arch: "%s"\n      uri: file://%s\n      sha256: "%s"\n      bin: kubectl-sshuttle\n      files:\n        - from: kubectl-sshuttle\n          to: .\n        - from: rushtle\n          to: .\n        - from: LICENSE\n          to: .\n' "$(KREW_OS)" "$(KREW_ARCH)" "$(abspath $(KREW_TARBALL))" "$$SHA" > $$TMPDIR/sshuttle.yaml; \
	  echo "manifest: $$TMPDIR/sshuttle.yaml"; \
	  kubectl krew uninstall sshuttle 2>/dev/null || true; \
	  kubectl krew install --manifest="$$TMPDIR/sshuttle.yaml" --archive="$(abspath $(KREW_TARBALL))"
	@echo "==> installed. Try: kubectl sshuttle --help"
	@echo "    rushtle binary: $$HOME/.krew/store/sshuttle/v0.0.0-local/rushtle"

krew-uninstall-local:
	-kubectl krew uninstall sshuttle

# In-tree smoke tests: drive `rushtle server` with python harnesses that
# craft ssnet frames. Verify TCP CONNECT+DATA echo and DNS_REQ resolution
# without requiring iptables / root.
rushtle-test: rushtle
	cd $(RUSHTLE_DIR) && python3 ../scripts/rushtle-smoketest.py
	cd $(RUSHTLE_DIR) && python3 ../scripts/rushtle-dns-smoketest.py
	cd $(RUSHTLE_DIR) && python3 ../scripts/rushtle-udp-smoketest.py
	cd $(RUSHTLE_DIR) && python3 ../scripts/rushtle-host-smoketest.py
	cd $(RUSHTLE_DIR) && python3 ../scripts/rushtle-bootstrap-smoketest.py
	cd $(RUSHTLE_DIR) && python3 ../scripts/rushtle-shim-smoketest.py
