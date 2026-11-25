// The row that sends a test notification.

import { useState } from "react";
import { Row } from "../components/SettingsRow";
import { testNotification } from "../shared/notify";

/** Sends a sample system notification, asking for permission first where the system wants it. */
export function TestNotificationRow() {
  const [result, setResult] = useState<string | null>(null);
  const send = async () => {
    setResult(null);
    const shown = await testNotification();
    if (!shown) setResult("Notifications are blocked. Allow them for Imperium in your system or browser settings.");
  };
  return (
    <Row label="Try a notification" description={result ?? "Show a sample system notification now."}>
      <button onClick={() => void send()}>Send a test</button>
    </Row>
  );
}
