FROM caddy:2.11.9-alpine@sha256:0bbb055bda6793c999bde78b1b5ce3e89f829fb031af2cf50056ea1246559fee
RUN command -v getcap >/dev/null && cp /usr/bin/caddy /usr/local/bin/caddy-relay && test -z "$(getcap /usr/local/bin/caddy-relay)"
COPY deploy/docker/relay.Caddyfile /etc/caddy/relay.Caddyfile
ENTRYPOINT ["/usr/local/bin/caddy-relay"]
CMD ["run", "--config", "/etc/caddy/relay.Caddyfile", "--adapter", "caddyfile"]
