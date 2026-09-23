#pragma once
// Small native readiness hook for managed hypridle, until upstream provides
// sd_notify readiness. No log scraping, shell helpers, or new link dependency.
#include <cstdlib>
#include <cstring>
#include <cstddef>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

inline bool shelllistManagedConfig() {
    const char* value = std::getenv("BAR_DAEMON_IDLE_MANAGED");
    return value && std::strcmp(value, "1") == 0;
}

inline bool shelllistNotifyReady() {
    const char* path = std::getenv("NOTIFY_SOCKET");
    if (!path || !*path)
        return true; // Running manually, outside a notify-type unit.
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    const size_t length = std::strlen(path);
    if (length >= sizeof(address.sun_path) || (path[0] != '/' && path[0] != '@'))
        return false;
    std::memcpy(address.sun_path, path, length + 1);
    const bool abstract = path[0] == '@';
    if (abstract)
        address.sun_path[0] = '\0';
    const int fd = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    if (fd < 0)
        return false;
    constexpr char message[] = "READY=1\nSTATUS=Idle listeners and session integration initialized";
    const auto sent = sendto(fd, message, sizeof(message) - 1, MSG_NOSIGNAL,
        reinterpret_cast<const sockaddr*>(&address),
        offsetof(sockaddr_un, sun_path) + length + (abstract ? 0 : 1));
    close(fd);
    return sent == sizeof(message) - 1;
}
