// Managed suspend listener control. Runs on Hypridle's existing event-loop thread;
// changing this listener never restarts lock/DPMS listeners or loses inhibitor cookies.
#include "Hypridle.hpp"
#include "../helpers/ShelllistReady.hpp"
#include "../config/ConfigManager.hpp"
#include <cstdlib>
#include <unistd.h>

void CHypridle::setupShelllistControl() {
    if (!shelllistManagedConfig())
        return;
    const char* generation = std::getenv("BAR_DAEMON_IDLE_GENERATION");
    const char* command = std::getenv("BAR_DAEMON_IDLE_COMMAND");
    const char* minutes = std::getenv("BAR_DAEMON_IDLE_MINUTES");
    if (!generation || !command || !minutes)
        throw std::runtime_error("Managed suspend environment is incomplete");
    m_shelllistBaseGeneration = generation;
    m_shelllistGeneration = generation;
    m_shelllistCommand = command;
    m_shelllistConnection = sdbus::createSessionBusConnection(sdbus::ServiceName{"org.laufan.Hypridle"});
    m_shelllistObject = sdbus::createObject(*m_shelllistConnection, sdbus::ObjectPath{"/org/laufan/Hypridle"});
    m_shelllistObject->addVTable(
        sdbus::registerMethod("GetState").implementedAs([this]() {
            // Drain compositor activity before attesting that an idle episode
            // is still valid. A stale local bool is not a pre-suspend barrier.
            if (wl_display_roundtrip(m_sWaylandState.display) < 0)
                throw sdbus::Error(sdbus::Error::Name{"org.laufan.Hypridle.Disconnected"}, "Compositor disconnected");
            return sdbus::Struct<uint32_t, std::string, uint32_t, uint64_t, bool>{getpid(), m_shelllistGeneration, m_shelllistMinutes, m_shelllistEpisode, m_shelllistIdle && m_iInhibitLocks == 0};
        }),
        sdbus::registerMethod("SetTimeout").implementedAs([this](std::string expected, uint32_t minutes) {
            if (expected != m_shelllistGeneration)
                throw sdbus::Error(sdbus::Error::Name{"org.laufan.Hypridle.StaleGeneration"}, "Idle generation changed");
            setShelllistTimeout(minutes);
        })
    ).forInterface(sdbus::InterfaceName{"org.laufan.Hypridle1"});
    setShelllistTimeout(std::stoul(minutes));
}

void CHypridle::setShelllistTimeout(uint32_t minutes) {
    if (minutes > 10080)
        throw sdbus::Error(sdbus::Error::Name{"org.laufan.Hypridle.InvalidTimeout"}, "Timeout must be 0-10080 minutes");
    if (minutes == m_shelllistMinutes)
        return;
    m_shelllistMinutes = minutes;
    m_shelllistGeneration = m_shelllistBaseGeneration + "-" + std::to_string(++m_shelllistRevision);
    resetShelllistListener();
}

void CHypridle::resetShelllistListener() {
    m_shelllistIdle = false;
    ++m_shelllistEpisode;
    auto& listener = m_shelllistListener;
    if (listener.notification) {
        listener.notification->sendDestroy();
        listener.notification.reset();
    }
    if (!m_shelllistMinutes)
        return;
    listener.onTimeout = m_shelllistCommand + " idle-sleep --sleep-minutes " + std::to_string(m_shelllistMinutes) + " --generation " + m_shelllistGeneration;
    static const auto IGNOREWAYLANDINHIBIT = g_pConfigManager->getValue<Hyprlang::INT>("general:ignore_wayland_inhibit");
    if (*IGNOREWAYLANDINHIBIT)
        listener.notification = makeShared<CCExtIdleNotificationV1>(m_sWaylandIdleState.notifier->sendGetInputIdleNotification(m_shelllistMinutes * 60000, m_sWaylandState.seat->resource()));
    else
        listener.notification = makeShared<CCExtIdleNotificationV1>(m_sWaylandIdleState.notifier->sendGetIdleNotification(m_shelllistMinutes * 60000, m_sWaylandState.seat->resource()));
    listener.notification->setIdled([this](CCExtIdleNotificationV1*) { onIdled(&m_shelllistListener); });
    listener.notification->setResumed([this](CCExtIdleNotificationV1*) { onResumed(&m_shelllistListener); });
}
