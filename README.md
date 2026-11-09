# throcc

Self-hosted voice and screen share, end-to-end encrypted.

![Watching a share in a room](docs/screens/focus.png)

- Rooms of up to 30 people with low-latency voice, echo cancellation and noise suppression.
- Screen share up to 1440p60 with its system audio, encoded in hardware.
- All media is end-to-end encrypted, and safety numbers let you verify who you talk to. The server relays the media but holds no key. It still sees who is in which room, and profiles and room names aren't encrypted.
- One server and one data directory. People join with its address and an invite code.

| ![Share your screen](docs/screens/share.png) | ![A safety number](docs/screens/person.png) |
| --- | --- |

## Client

Download it from the [latest release](https://github.com/nylyk/throcc/releases/latest):

- **macOS 14 or newer:** `throcc.dmg`
- **Windows:** `throcc-setup.exe`. It isn't code-signed yet, so SmartScreen asks first: More info, then Run anyway.
- **Linux:** `throcc.flatpak`, installed with `flatpak install --user throcc.flatpak`
- **Nix:** `nix run github:nylyk/throcc`

Join with the server's address and an invite code, and check that the fingerprint the client shows matches the server's. The client tells you when a new version is out; turn that off in Settings, About.

## Server

The server listens on UDP 8476 (QUIC). Open that port, and forward it if the server is behind NAT.

```sh
docker run -d --name throcc --restart unless-stopped -p 8476:8476/udp -v throcc-data:/data ghcr.io/nylyk/throcc:0.1
```

Or with Docker Compose:

```yaml
services:
  throcc:
    image: ghcr.io/nylyk/throcc:0.1
    container_name: throcc
    restart: unless-stopped
    ports:
      - 8476:8476/udp
    volumes:
      - throcc-data:/data

volumes:
  throcc-data:
```

The image runs as user `65532`, so a bind mount instead of the volume must be owned by `65532:65532`.

Then make the first invite. It prints the code and the server's fingerprint, and whoever joins with it becomes an admin. Admins make more invites in Server Settings.

```sh
docker exec throcc /throcc-server admin-invite --data /data
```

Stay on a minor version like `:0.1`: a new one may need newer clients. Back up the data volume. It holds the database and `server_key`, and without `server_key` every client has to join again.

## Build from source

```sh
cargo build --release -p throcc-client --features ffmpeg
cargo build --release -p throcc-server
```

Screen share needs ffmpeg 8; without `--features ffmpeg` the client does voice only. On macOS, `brew install ffmpeg@8` and set `FFMPEG_PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`. On Linux, install the packages listed in `.github/workflows/ci.yml` and ffmpeg 8's development files. With clang 21 or newer, generate ffmpeg's bindings with `packaging/ffmpeg/bindings.sh <ffmpeg prefix> binding.rs` and build with `FFMPEG_DLL_PATH=<prefix>/lib FFMPEG_BINDING_PATH=binding.rs`.

The release packages come from the scripts in `packaging/`, run the way `.github/workflows/packaging.yml` runs them.

## License

[AGPL-3.0-or-later](LICENSE)
