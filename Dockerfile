FROM rustlang/rust:nightly as build

WORKDIR /usr/src/shipwright
COPY . ./

ARG GITHUB_TOKEN
RUN git config --global url."https://${GITHUB_TOKEN}@github.com/".insteadOf "https://github.com/"

RUN rustup update nightly; rustup default nightly

RUN cargo build --release

FROM debian:buster-slim

COPY --from=build /usr/src/shipwright/target/release/shipwright /app/shipwright
WORKDIR /app

EXPOSE 8000

RUN apt-get update && apt-get install libssl-dev ca-certificates -y && rm -rf /var/lib/apt/lists/*

CMD ["/app/shipwright"]
