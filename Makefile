SHELL := /bin/bash
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH := amd64
STAGE := build/deb
DIST := dist
GLIBC := $(shell objdump -T target/release/aether 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1 | sed 's/GLIBC_//')
ifeq ($(GLIBC),)
GLIBC := 2.39
endif

.PHONY: release check deb clean

release:
	cargo build --release

check: release
	desktop-file-validate packaging/aether.desktop
	appstreamcli validate --no-net packaging/aether.metainfo.xml

deb: release
	rm -rf "$(STAGE)"
	install -Dm755 target/release/aether "$(STAGE)/usr/bin/aether"
	install -Dm644 packaging/aether.desktop "$(STAGE)/usr/share/applications/aether.desktop"
	install -Dm644 packaging/aether.metainfo.xml "$(STAGE)/usr/share/metainfo/io.github.pengtang1024.Aether.metainfo.xml"
	for size in 32 48 64 128 256 512; do \
		install -Dm644 "assets/logo-icon-$$size.png" \
			"$(STAGE)/usr/share/icons/hicolor/$${size}x$${size}/apps/aether.png"; \
	done
	install -Dm644 LICENSE "$(STAGE)/usr/share/doc/aether/copyright"
	install -Dm644 THIRD_PARTY_NOTICES.md "$(STAGE)/usr/share/doc/aether/THIRD_PARTY_NOTICES.md"
	mkdir -p "$(STAGE)/DEBIAN" "$(DIST)"
	sed -e "s/@VERSION@/$(VERSION)/" \
	    -e "s/@LIBC@/$(GLIBC)/" \
	    -e "s/@SIZE@/$$(du -sk "$(STAGE)/usr" | cut -f1)/" \
	    packaging/debian/control.in > "$(STAGE)/DEBIAN/control"
	dpkg-deb --build --root-owner-group "$(STAGE)" "$(DIST)/aether_$(VERSION)_$(ARCH).deb"

clean:
	rm -rf build dist
