FROM ubuntu:24.04@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /data \
    && chown 10001:10001 /data

COPY --chown=10001:10001 dist/discord-mcp /usr/local/bin/discord-mcp
USER 10001:10001
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/discord-mcp"]
CMD ["serve"]
