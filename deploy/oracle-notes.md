# Oracle Cloud Always Free notes

Lumberroom runs on any Linux VM. These are the Oracle-specific traps.

## Shape

Ampere A1 (arm64), up to 4 OCPU and 24 GB RAM on the Always Free tier. Everything here is built
and tested for arm64: the pgvector image is multi-arch, and the embedding model was measured on
`linux/arm64` before it was chosen (see DECISIONS.md).

## The iptables trap

Oracle's Ubuntu and Oracle Linux images ship iptables rules that REJECT everything except SSH.
Opening 80 and 443 in the VCN security list is not enough. The port stays closed and Caddy fails
its ACME challenge with a timeout that looks like a DNS problem.

Ubuntu images:

```bash
sudo iptables -I INPUT 6 -m state --state NEW -p tcp --dport 80 -j ACCEPT
sudo iptables -I INPUT 6 -m state --state NEW -p tcp --dport 443 -j ACCEPT
sudo netfilter-persistent save
```

Oracle Linux images use firewalld:

```bash
sudo firewall-cmd --permanent --add-service=http
sudo firewall-cmd --permanent --add-service=https
sudo firewall-cmd --reload
```

`deploy/install.sh` handles the ufw and firewalld cases and warns when it sees REJECT rules it
did not add.

## Security list

Ingress on 443 from 0.0.0.0/0, ingress on 80 from 0.0.0.0/0 for ACME, and SSH from wherever you
connect. Nothing else. Postgres is published on 127.0.0.1 only and the MCP server binds
127.0.0.1 as well, with Caddy the only process listening publicly.

## Boot volume

The default 50 GB is plenty. The image measures about 590MB with the model baked in (a 36MB
binary, ~210MB of embedding weights, the rest base OS and runtime libraries, see docs/traps.md for
why the weights are bigger than they look like they should be), and Postgres growth is dominated
by 768-dimension vectors at about 3 KB per memory.

## Reclamation

Always Free instances can be reclaimed when idle. A box serving session bootstraps stays busy
enough in practice, but if you care, keep the daily backup cron enabled and store a copy off the
box.
