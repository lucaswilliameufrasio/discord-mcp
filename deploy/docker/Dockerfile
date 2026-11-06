# Supply an operator-reviewed runtime image by immutable digest. It must provide
# a compatible libc and a usable system CA store for the supplied binary.
ARG RUNTIME_IMAGE
FROM ${RUNTIME_IMAGE}
ARG BINARY_PATH=target/release/discord-mcp
COPY ${BINARY_PATH} /usr/local/bin/discord-mcp
ENTRYPOINT ["/usr/local/bin/discord-mcp"]
CMD ["serve"]
