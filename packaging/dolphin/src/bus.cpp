#include "bus.h"

#include <QDBusArgument>
#include <QDBusMessage>

namespace syncfast
{

namespace
{

Pairs readArray(const QDBusMessage &reply, const QLatin1String &signature, bool withDetail)
{
    if (reply.type() != QDBusMessage::ReplyMessage || reply.signature() != signature) {
        return {};
    }
    const QDBusArgument array = reply.arguments().value(0).value<QDBusArgument>();
    Pairs result;
    array.beginArray();
    while (!array.atEnd()) {
        QString first;
        QString second;
        array.beginStructure();
        array >> first >> second;
        if (withDetail) {
            QString detail;
            array >> detail;
        }
        array.endStructure();
        result.append({first, second});
    }
    array.endArray();
    return result;
}

}

Pairs readRoots(const QDBusMessage &reply)
{
    return readArray(reply, QLatin1String("a(ss)"), false);
}

Pairs readEntries(const QDBusMessage &reply)
{
    return readArray(reply, QLatin1String("a(sss)"), true);
}

}
