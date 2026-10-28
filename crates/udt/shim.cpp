// C API over the vendored UDT4 library: just enough for a receive-only
// stream client (connect, recv with timeout, close). Used from src/lib.rs.

#include <arpa/inet.h>
#include <netinet/in.h>
#include <cstring>
#include <mutex>

#include "udt.h"

namespace {

std::once_flag started;

void copy_error(char* err, int err_len) {
  if (err == nullptr || err_len <= 0) return;
  std::strncpy(err, UDT::getlasterror().getErrorMessage(), err_len - 1);
  err[err_len - 1] = '\0';
}

}  // namespace

extern "C" {

// Connect to an IPv4 address (host byte order) and port. Returns the socket,
// or -1 with the reason in `err`. `recv_timeout_ms` bounds each udt_shim_recv.
int udt_shim_connect(uint32_t ipv4, uint16_t port, int recv_timeout_ms, char* err, int err_len) {
  std::call_once(started, [] { UDT::startup(); });
  UDTSOCKET s = UDT::socket(AF_INET, SOCK_STREAM, 0);
  if (s == UDT::INVALID_SOCK) {
    copy_error(err, err_len);
    return -1;
  }
  UDT::setsockopt(s, 0, UDT_RCVTIMEO, &recv_timeout_ms, sizeof recv_timeout_ms);
  sockaddr_in peer{};
  peer.sin_family = AF_INET;
  peer.sin_port = htons(port);
  peer.sin_addr.s_addr = htonl(ipv4);
  if (UDT::connect(s, reinterpret_cast<sockaddr*>(&peer), sizeof peer) == UDT::ERROR) {
    copy_error(err, err_len);
    UDT::close(s);
    return -1;
  }
  return s;
}

// Receive up to `len` bytes. Returns the count, 0 on timeout, or -1 with the
// reason in `err` (connection lost or closed).
int udt_shim_recv(int sock, char* buf, int len, char* err, int err_len) {
  int n = UDT::recv(sock, buf, len, 0);
  if (n != UDT::ERROR) return n;
  if (UDT::getlasterror().getErrorCode() == CUDTException::ETIMEOUT) return 0;
  copy_error(err, err_len);
  return -1;
}

void udt_shim_close(int sock) { UDT::close(sock); }

}  // extern "C"
