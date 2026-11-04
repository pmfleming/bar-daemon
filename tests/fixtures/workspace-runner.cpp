// A minimal Qt runner with distinct desktop identities for the real-window tests.
// The generic qml executable fixes its desktop ID to org.qt-project.qml.
#include <QtGui/QGuiApplication>
#include <QtQml/QQmlApplicationEngine>

int main(int argc, char **argv) {
    QGuiApplication app(argc, argv);
    app.setQuitOnLastWindowClosed(false);
    QQmlApplicationEngine engine;
    const auto args = app.arguments();
    for (const auto &arg : args) {
        if (arg.startsWith("icon-app-")) app.setDesktopFileName(arg);
    }
    const auto imports = args.indexOf("-I");
    if (imports < 0 || imports + 2 >= args.size()) return 2;
    engine.addImportPath(args[imports + 1]);
    engine.load(QUrl::fromLocalFile(args[imports + 2]));
    if (engine.rootObjects().isEmpty()) return 3;
    return app.exec();
}
