FROM ubuntu:24.04

# Prevent interactive prompts during package installation
ENV DEBIAN_FRONTEND=noninteractive

# Install dependencies
RUN apt-get update && apt-get install -y \
    git \
    curl \
    python3 \
    python3-pip \
    python3-venv \
    libusb-1.0-0 \
    libudev-dev \
    pkg-config \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# Install Rust via rustup
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"

# Install espup and ESP toolchain
RUN cargo install --locked espup ldproxy espflash
RUN espup install

# Source the ESP environment in bashrc
RUN echo '. /root/export-esp.sh' >> /root/.bashrc

ENV LIBCLANG_PATH="/root/.rustup/toolchains/esp/xtensa-esp32-elf-clang/esp-18.1.2_20240912/esp-clang/lib"

WORKDIR /project

CMD ["/bin/bash"]
