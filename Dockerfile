FROM rust as build

WORKDIR /usr/src/shipwright
COPY . ./

RUN cargo build --release

FROM debian:buster-slim

COPY --from=build /usr/src/shipwright/target/release/shipwright /app/shipwright
WORKDIR /app

EXPOSE 8000

RUN apt-get update && apt-get install libssl-dev ca-certificates -y && rm -rf /var/lib/apt/lists/*

CMD ["/app/shipwright"]
