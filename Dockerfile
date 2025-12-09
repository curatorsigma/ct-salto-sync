FROM rust:1.88-alpine AS builder
RUN apk add --no-cache build-base
WORKDIR /usr/src/salto-sync/
COPY . .
RUN SQLX_OFFLINE=true cargo build --release
CMD ["salto-sync"]

FROM alpine:latest
# tz should be set in docker compose or similar
RUN apk add --no-cache tzdata
WORKDIR /salto-sync
COPY --from=builder /usr/src/salto-sync/target/release/salto-sync ./
CMD ["./salto-sync"]

