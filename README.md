# Foreman Observability Agent
Foreman is a proprietary observability agent to actively monitor the health of your infrastructure. Upon failure, it will send you a persistent system notification via `notify-rust`.

Currently supported & future planned health checks:

- [x] ICMP ping
- [ ] HTTP GET
- [x] SSH
- [ ] socket.io Server
- [ ] MongoDB

Additional health checks may be added in the future, but these are the ones I personally require for my own infrastructure.
