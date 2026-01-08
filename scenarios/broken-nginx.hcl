scenario "broken-nginx" {
  description = "Fix a misconfigured nginx server"

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

  probe "nginx-running" {
    type    = "service"
    service = "nginx"
    state   = "running"
    description = "Nginx should be running to serve the default site"
  }

  probe "port-80-open" {
    type  = "port"
    port  = 80
    state = "listening"
    description = "HTTP port 80 should be listening"
  }

  probe "default-site-enabled" {
    type   = "file_exists"
    path   = "/etc/nginx/sites-enabled/default"
    exists = true
    description = "Default site should be enabled in /etc/nginx/sites-enabled"
  }

  vm "webserver" {
    cpu    = 2
    memory = 2048
    disk   = 10
    image  = "ubuntu-24.04"

    cloud_init {
      packages = ["nginx", "curl"]
    }

    step "break-nginx" {
      systemctl {
        unit   = "nginx"
        action = "stop"
      }

      file_delete {
        path = "/etc/nginx/sites-enabled/default"
      }
    }

    probes = ["nginx-running", "port-80-open", "default-site-enabled"]
  }
}
