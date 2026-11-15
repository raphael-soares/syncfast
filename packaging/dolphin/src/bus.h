#pragma once

#include "status.h"

class QDBusMessage;

namespace syncfast
{

Pairs readRoots(const QDBusMessage &reply);
Pairs readEntries(const QDBusMessage &reply);

}
