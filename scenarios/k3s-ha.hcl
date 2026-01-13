scenario "k3s-ha" {
  description = "Three control-plane k3s nodes on a dedicated private network"

  image "ubuntu-24.04" {
    source {
      arch     = "amd64"
      url      = "https://cloud-images.ubuntu.com/releases/releases/24.04/release-20251001/ubuntu-24.04-server-cloudimg-amd64.img"
      checksum = "sha256:02dc186dc491514254df58df29a5877c93ca90ac790cc91a164ea90ed3a0bb05"
    }
    source {
      arch     = "arm64"
      url      = "https://cloud-images.ubuntu.com/releases/releases/24.04/release-20251001/ubuntu-24.04-server-cloudimg-arm64.img"
      checksum = "sha256:88b381e23c422d4c625d8fb24d3d5bd03339c642c77bcb75f317cbef0dedd50f"
    }
  }

  probe "k3s-service" {
    phase   = "boot"
    type    = "service"
    service = "k3s"
    state   = "running"
    description = "k3s service should be running on each control plane node"
  }

  probe "api-port" {
    phase = "boot"
    type  = "port"
    port  = 6443
    state = "listening"
    description = "Kubernetes API server should be listening on 6443"
  }

  probe "nodes-ready" {
    phase          = "boot"
    type           = "k8s_nodes_ready"
    kubeconfig     = "/etc/rancher/k3s/k3s.yaml"
    expected_ready = 3
    description = "All three control-plane nodes should report Ready"
  }

  probe "ping-node1" {
    phase     = "boot"
    type       = "tcp_ping"
    host       = "k3s-1"
    timeout_ms = 2000
    description = "k3s-1 should be reachable over the private network"
  }

  probe "ping-node2" {
    phase     = "boot"
    type       = "tcp_ping"
    host       = "k3s-2"
    timeout_ms = 2000
    description = "k3s-2 should be reachable over the private network"
  }

  probe "ping-node3" {
    phase     = "boot"
    type       = "tcp_ping"
    host       = "k3s-3"
    timeout_ms = 2000
    description = "k3s-3 should be reachable over the private network"
  }

  probe "echo-svc-endpoints" {
    type       = "k8s_endpoints_nonempty"
    kubeconfig = "/etc/rancher/k3s/k3s.yaml"
    namespace  = "intar-test"
    name       = "echo-svc"
    description = "echo-svc should have endpoints after fixing its selector"
  }

  vm "k3s-1" {
    cpu    = 1
    memory = 2048
    disk   = 10
    image  = "ubuntu-24.04"

    cloud_init {}

    step "k3s-bootstrap" {
      file_write {
        path        = "/usr/local/bin/k3s-bootstrap.sh"
        permissions = "0755"
        content     = <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec > /var/log/k3s-bootstrap.log 2>&1

CLUSTER_IF="enp0s2"
export K3S_TOKEN="intar-cluster-token"

ip link set "$CLUSTER_IF" up || true

mkdir -p /etc/rancher/k3s
cat > /etc/rancher/k3s/registries.yaml <<'REGISTRY_EOF'
mirrors:
  docker.io:
  registry.k8s.io:
REGISTRY_EOF

# Most Ubuntu cloud images ship curl; install only if missing to avoid slow apt runs.
if ! command -v curl >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y curl
fi

# Wait briefly for the cluster NIC to get its static address.
NODE_IP=""
for _ in $(seq 1 30); do
  NODE_IP=$(ip -o -4 addr show dev "$CLUSTER_IF" 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1 || true)
  [ -n "$NODE_IP" ] && break
  sleep 1
done
[ -n "$NODE_IP" ] || { echo "No IPv4 on $CLUSTER_IF"; exit 1; }

COMMON_ARGS="server --embedded-registry --flannel-iface $CLUSTER_IF --node-ip $NODE_IP --advertise-address $NODE_IP --tls-san $HOSTNAME --tls-san $HOSTNAME.intar --tls-san k3s-server --tls-san k3s-server.intar"

if [ "$HOSTNAME" = "k3s-1" ]; then
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="$COMMON_ARGS --cluster-init" sh -
else
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="$COMMON_ARGS --server https://k3s-server:6443" sh -
fi
EOF
      }

      command {
        cmd = "/usr/local/bin/k3s-bootstrap.sh"
      }
    }

    step "seed-workload" {
      command {
        cmd = <<-EOF
          export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
          for _ in $(seq 1 60); do
            if kubectl get nodes --no-headers 2>/dev/null | awk '$2=="Ready"{c++} END{exit !(c==3)}'; then
              break
            fi
            sleep 5
          done
          kubectl taint nodes --all node-role.kubernetes.io/control-plane- || true
          kubectl taint nodes --all node-role.kubernetes.io/master- || true
        EOF
      }

      k8s_namespace {
        name    = "intar-test"
      }

      k8s_deployment {
        name           = "echo"
        namespace      = "intar-test"
        image          = "nginx:1.27-alpine"
        replicas       = 1
        labels         = { app = "echo" }
        container_port = 80
      }

      k8s_service {
        name        = "echo-svc"
        namespace   = "intar-test"
        selector    = { app = "echo-typo" }
        port        = 80
        target_port = 80
      }
    }

    probes = ["k3s-service", "api-port", "nodes-ready", "ping-node2", "ping-node3", "echo-svc-endpoints"]
  }

  vm "k3s-2" {
    cpu    = 1
    memory = 2048
    disk   = 10
    image  = "ubuntu-24.04"

    cloud_init {}

    step "k3s-bootstrap" {
      file_write {
        path        = "/usr/local/bin/k3s-bootstrap.sh"
        permissions = "0755"
        content     = <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec > /var/log/k3s-bootstrap.log 2>&1

CLUSTER_IF="enp0s2"
export K3S_TOKEN="intar-cluster-token"

ip link set "$CLUSTER_IF" up || true

mkdir -p /etc/rancher/k3s
cat > /etc/rancher/k3s/registries.yaml <<'REGISTRY_EOF'
mirrors:
  docker.io:
  registry.k8s.io:
REGISTRY_EOF

if ! command -v curl >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y curl
fi

NODE_IP=""
for _ in $(seq 1 30); do
  NODE_IP=$(ip -o -4 addr show dev "$CLUSTER_IF" 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1 || true)
  [ -n "$NODE_IP" ] && break
  sleep 1
done
[ -n "$NODE_IP" ] || { echo "No IPv4 on $CLUSTER_IF"; exit 1; }

COMMON_ARGS="server --embedded-registry --flannel-iface $CLUSTER_IF --node-ip $NODE_IP --advertise-address $NODE_IP --tls-san $HOSTNAME --tls-san $HOSTNAME.intar --tls-san k3s-server --tls-san k3s-server.intar"

if [ "$HOSTNAME" = "k3s-1" ]; then
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="$COMMON_ARGS --cluster-init" sh -
else
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="$COMMON_ARGS --server https://k3s-server:6443" sh -
fi
EOF
      }

      command {
        cmd = "/usr/local/bin/k3s-bootstrap.sh"
      }
    }

    probes = ["k3s-service", "api-port", "ping-node1", "ping-node3"]
  }

  vm "k3s-3" {
    cpu    = 1
    memory = 2048
    disk   = 10
    image  = "ubuntu-24.04"

    cloud_init {}

    step "k3s-bootstrap" {
      file_write {
        path        = "/usr/local/bin/k3s-bootstrap.sh"
        permissions = "0755"
        content     = <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec > /var/log/k3s-bootstrap.log 2>&1

CLUSTER_IF="enp0s2"
export K3S_TOKEN="intar-cluster-token"

ip link set "$CLUSTER_IF" up || true

mkdir -p /etc/rancher/k3s
cat > /etc/rancher/k3s/registries.yaml <<'REGISTRY_EOF'
mirrors:
  docker.io:
  registry.k8s.io:
REGISTRY_EOF

if ! command -v curl >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y curl
fi

NODE_IP=""
for _ in $(seq 1 30); do
  NODE_IP=$(ip -o -4 addr show dev "$CLUSTER_IF" 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1 || true)
  [ -n "$NODE_IP" ] && break
  sleep 1
done
[ -n "$NODE_IP" ] || { echo "No IPv4 on $CLUSTER_IF"; exit 1; }

COMMON_ARGS="server --embedded-registry --flannel-iface $CLUSTER_IF --node-ip $NODE_IP --advertise-address $NODE_IP --tls-san $HOSTNAME --tls-san $HOSTNAME.intar --tls-san k3s-server --tls-san k3s-server.intar"

if [ "$HOSTNAME" = "k3s-1" ]; then
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="$COMMON_ARGS --cluster-init" sh -
else
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="$COMMON_ARGS --server https://k3s-server:6443" sh -
fi
EOF
      }

      command {
        cmd = "/usr/local/bin/k3s-bootstrap.sh"
      }
    }

    probes = ["k3s-service", "api-port", "ping-node1", "ping-node2"]
  }
}
