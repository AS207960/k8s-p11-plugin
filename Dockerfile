FROM debian:stable

RUN apt-get update && apt-get install -y p11-kit-modules gnutls-bin
