FROM rust:slim as build
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build
COPY . .
RUN cargo build --release --all-features --target x86_64-unknown-linux-musl

FROM linuxserver/wireguard
ENV TZ=Etc/UTC
EXPOSE 80
ARG BUILDARCH

RUN apk add --no-cache curl

# Install cloudflared
RUN curl -L -o /usr/bin/cloudflared "https://github.com/cloudflare/cloudflared/releases/download/2024.6.1/cloudflared-linux-amd64" && chmod +x /usr/bin/cloudflared

# Install AWS SDK
# TODO

# Install outpost
COPY --from=build /build/target/x86_64-unknown-linux-musl/release/outpost /usr/bin/outpost

ENTRYPOINT [ "/usr/bin/outpost" ]
