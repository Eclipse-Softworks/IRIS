# The IRIS package registry

`iris pkg` installs registry dependencies from here by default: this
directory, served from the repository's raw files
(`https://raw.githubusercontent.com/Eclipse-Softworks/IRIS/master/registry`).

```toml
[dependencies]
json = { registry = "json", version = "^1.0" }
```

## Layout

- `index.json` — every package, its versions, and for each version the
  archive's path (relative to this directory) and its SHA-256 checksum.
- `pkgs/NAME-VERSION.tar.gz` — the archives.

## Publishing

From the package's directory, into a checkout of this repository:

```sh
iris pkg publish --registry path/to/IRIS/registry
```

Then open a pull request with the new archive and the updated index. A
published version is never replaced: publish a new version instead.

## Your own registry

Any directory laid out like this one is a registry: point `IRIS_REGISTRY`,
or `registry` in `[package]`, at it, at a `file://` URL, or at an HTTP(S)
server that serves it (`iris pkg serve DIR` is one).
