PREFIX ?= /usr/local
UNAME_S := $(shell uname -s)

build:
	cargo build --release

install: build
	install -m 755 target/release/zestful $(PREFIX)/bin/zestful
ifeq ($(UNAME_S),Darwin)
	codesign -s - $(PREFIX)/bin/zestful
endif

uninstall:
	rm -f $(PREFIX)/bin/zestful

.PHONY: build install uninstall
