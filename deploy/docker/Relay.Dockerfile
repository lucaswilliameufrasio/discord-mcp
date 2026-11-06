# Copy without preserving extended attributes: the relay listens on an
# unprivileged port and requires no Linux capabilities.
ARG CADDY_IMAGE
FROM ${CADDY_IMAGE}
RUN command -v getcap >/dev/null && cp /usr/bin/caddy /usr/local/bin/caddy-relay && test -z "$(getcap /usr/local/bin/caddy-relay)"
ENTRYPOINT ["/usr/local/bin/caddy-relay"]
