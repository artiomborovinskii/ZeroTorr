FROM alpine:3.20
COPY zerotorr-linux-amd64 /usr/local/bin/zerotorr
RUN chmod +x /usr/local/bin/zerotorr
EXPOSE 8090
VOLUME /data
ENTRYPOINT ["/usr/local/bin/zerotorr", "-p", "8090", "-d", "/data"]
