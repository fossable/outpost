FROM rust as build
WORKDIR /build
COPY . .
RUN cargo build --release --all-features

FROM debian:bookworm
EXPOSE 80
ARG BUILDARCH
ENV TZ=Etc/UTC

RUN apt-get update \
    && apt-get install -y ca-certificates curl tzdata \
    && rm -rf /var/lib/apt/lists/*

# Install cloudflared
RUN curl -o /usr/bin/cloudflared "https://github.com/cloudflare/cloudflared/releases/download/2024.6.0/cloudflared-linux-${BUILDARCH}" && chmod /usr/bin/cloudflared

# Install AWS SDK
# TODO

# Install outpost
COPY --from=build /build/target/release/outpost /usr/bin/outpost

ENTRYPOINT [ "/usr/bin/outpost" ]
