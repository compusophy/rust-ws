FROM rust:slim

# Install dependencies
RUN apt-get update && \
    apt-get install -y libsqlite3-dev ca-certificates pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the code
COPY src/ src/
COPY templates/ templates/
COPY static/ static/
COPY Cargo.toml ./

# Build the application
RUN cargo build --release

# Set the environment variables
ENV ROCKET_ADDRESS=0.0.0.0
ENV ROCKET_PORT=8000

EXPOSE 8000

CMD ["./target/release/example-todo-app-rust-htmx"] 