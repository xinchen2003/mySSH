# 轴一 1.3：CI E2E 用 OpenSSH 服务端。
# tester/e2e-pass 密码认证；root 密码 e2e-root 供 su 密码应答用例（PAM 真实链路）。
FROM lscr.io/linuxserver/openssh-server:latest
ENV PASSWORD_ACCESS=true \
    USER_NAME=tester \
    USER_PASSWORD=e2e-pass
RUN echo 'root:e2e-root' | chpasswd
