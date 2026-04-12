BINARY := kubectl-sshuttle
GOBIN  ?= $(shell go env GOPATH)/bin

.PHONY: build test install clean

build:
	go build -o $(BINARY) .

test:
	go test ./... -v

install: build
	install -m 755 $(BINARY) $(GOBIN)/$(BINARY)

clean:
	rm -f $(BINARY)
