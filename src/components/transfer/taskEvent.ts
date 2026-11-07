import { errorText } from '../../api/network';
import legacyTaskEvents from '../../i18n/locales/zh-CN/legacyTaskEvents.json';

type Translate = (key: string, options?: Record<string, string | number>) => string;
type LegacyEventKey = keyof typeof legacyTaskEvents;

function eventMessage(value: string): string {
  return value.match(/^(\d{4}-\d{2}-\d{2}T\S+?)(\s+)(.*)$/)?.[3] ?? value;
}

function matchLegacyEvent(message: string): { key: LegacyEventKey; args: Record<string, string> } | undefined {
  for (const [key, templates] of Object.entries(legacyTaskEvents) as [LegacyEventKey, string[]][]) {
    for (const template of templates) {
      const names: string[] = [];
      const pattern = template.split(/(\{\{\s*[^}\s]+\s*\}\})/g).map(part => {
        const placeholder = part.match(/^\{\{\s*([^}\s]+)\s*\}\}$/);
        if (placeholder) {
          names.push(placeholder[1]);
          return '(.+?)';
        }
        return part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      }).join('');
      const match = message.match(new RegExp(`^${pattern}$`));
      if (match) {
        return {
          key,
          args: Object.fromEntries(names.map((name, index) => [name, match[index + 1]])),
        };
      }
    }
  }
  return undefined;
}

export function isHiddenLegacyTaskEvent(value: string): boolean {
  const key = matchLegacyEvent(eventMessage(value))?.key;
  return key === 'userAction' || key === 'cancellationCleanup';
}

const exactEvents: Record<string, string> = {
  'Task paused after the previous shutdown': 'previouslyPaused',
  'The process ended unexpectedly. Waiting to restore the task connection': 'unexpectedExit',
  'The user exited from the tray. The task is offline and can be resumed manually': 'exitedFromTray',
  'The receiver declined the transfer': 'receiverRejected',
  'Acceptance confirmed. Receiving started': 'receiveStarted',
  'The peer was notified to stop automatic reconnection because the app is exiting': 'appExitNotified',
  'All files were saved and committed. Transfer completion was acknowledged': 'filesCommitted',
  'Saving to the selected folder failed. Files remain in the app and can be saved again': 'saveFailed',
  'A newer source file replaced the previous version. Receiving continues': 'sourceReplaced',
  'The sender reconnected. Receiving resumed automatically': 'senderReconnected',
  'The accepted receive task resumed after reconnecting': 'receiveResumed',
  'File data transfer started': 'sendingStarted',
  'The device is temporarily offline. The task is paused; resume when it is online': 'deviceTemporarilyOffline',
  'Received the receiver’s file-write receipt': 'diskReceipt',
  'The receiver cancelled the task': 'receiverCancelled',
  'The receiver declined the task': 'receiverRejectedTask',
  'The receiver paused the task. Waiting for it to resume': 'receiverPaused',
  'The receiver has not received the resume notice. Synchronizing again': 'resyncing',
  'Connecting to the sender’s status channel': 'connectingState',
  'Connection interrupted': 'connectionInterrupted',
  'The receiver is ready. Scanning and sending the manifest in parts': 'receiverReady',
  'The receiver already has this task. Synchronizing status before completing the manifest': 'syncExisting',
  'Sender identity verified. Receiving the file manifest': 'identityVerified',
  'The source file changed. Resuming will retransmit the current version': 'sourceChanged',
  'The source file is missing. You can finish with the received files or cancel': 'sourceMissing',
  'The source file is temporarily unavailable. Resume to try again': 'sourceUnavailable',
  'Available files were kept. The task is partially complete; unreceived files are listed as abandoned': 'partialCompleted',
  'Waiting for the receiver to continue': 'waitingForReceiver',
};

export function translateTaskEvent(value: string, t: Translate): string {
  const match = value.match(/^(\d{4}-\d{2}-\d{2}T\S+?)(\s+)(.*)$/);
  const message = match?.[3] ?? value;
  const legacy = matchLegacyEvent(message);
  if (legacy) {
    if (legacy.key === 'skippedSourceWarning' || legacy.key === 'duplicateDestinationSkipped') {
      return t(`transfer:tasks.${legacy.key}`, legacy.args);
    }
    if (['manualReconnectFailed', 'cacheWriteFailed', 'writeFailed'].includes(legacy.key) && legacy.args.error) {
      legacy.args.error = errorText(legacy.args.error);
    }
    return t(`transfer:tasks.events.${legacy.key}`, legacy.args);
  }
  const sourceIssues = [
    ['The source file changed. Resuming will retransmit the current version', 'sourceChangedWithFile'],
    ['The source file is missing. You can finish with the received files or cancel', 'sourceMissingWithFile'],
    ['The source file is temporarily unavailable. Resume to try again', 'sourceUnavailableWithFile'],
  ] as const;
  for (const [prefix, key] of sourceIssues) {
    const marker = `${prefix}: `;
    if (message.startsWith(marker)) {
      return t(`transfer:tasks.events.${key}`, { file: message.slice(marker.length) });
    }
  }

  const exact = exactEvents[message];
  if (exact) return t(`transfer:tasks.events.${exact}`);

  let dynamic = message.match(/^The receiver already has this task\. Continuing the remaining manifest from entry (\d+)$/i);
  if (dynamic) return t('transfer:tasks.events.continueManifest', { cursor: dynamic[1] });

  dynamic = message.match(/^Manifest scan complete: (\d+) files · (\d+) folders?$/i);
  if (dynamic) return t('transfer:tasks.events.manifestScanned', { files: dynamic[1], folders: dynamic[2] });

  dynamic = message.match(/^Sending manifest in parts: (\d+) entries scanned$/i);
  if (dynamic) return t('transfer:tasks.events.manifestProgress', { count: dynamic[1] });

  dynamic = message.match(/^Transfer connection lost\. Retrying in (.+?) \(attempt (\d+)\)$/i);
  if (dynamic) return t('transfer:tasks.events.retrying', { delay: dynamic[1], attempt: dynamic[2] });

  dynamic = message.match(/^Manual reconnect failed: (.*)$/i);
  if (dynamic) return t('transfer:tasks.events.manualReconnectFailed', { error: errorText(dynamic[1]) });

  dynamic = message.match(/^File needs to be retransferred: (.*)$/i);
  if (dynamic) return t('transfer:tasks.events.fileRetransfer', { path: dynamic[1] });

  dynamic = message.match(/^Cache relay write failed: (.*)$/i);
  if (dynamic) return t('transfer:tasks.events.cacheWriteFailed', { error: errorText(dynamic[1]) });

  dynamic = message.match(/^Write failed: (.*)$/i);
  if (dynamic) return t('transfer:tasks.events.writeFailed', { error: errorText(dynamic[1]) });

  dynamic = message.match(/^(.+), skipped: (.*)$/i);
  if (dynamic) return t('transfer:tasks.events.skippedPath', { reason: errorText(dynamic[1]), path: dynamic[2] });

  if (/^Manifest scan incomplete; continuing with remaining entries$/i.test(message)) {
    return t('transfer:tasks.events.continueScan');
  }

  dynamic = message.match(/^Repairing (\d+) files from the original manifest$/i);
  if (dynamic) return t('transfer:tasks.events.repairFiles', { count: dynamic[1] });

  dynamic = message.match(/^Using (.+) \+ TLS 1\.3 via (.+)$/i);
  if (dynamic) return t('transfer:tasks.events.transport', { transport: dynamic[1], route: dynamic[2] });

  dynamic = message.match(/^Synchronized peer task status: (.*)$/i);
  if (dynamic) {
    const statusKeys: Record<string, string> = {
      resumed: 'resumed', paused: 'paused', finish: 'finish', offline: 'offline',
      cancelled: 'cancelled', rejected: 'rejected',
    };
    const statusKey = statusKeys[dynamic[1].toLowerCase()];
    const status = statusKey ? t(`transfer:tasks.peerStatusValues.${statusKey}`) : t('transfer:tasks.events.unknownPeerStatus');
    return t('transfer:tasks.events.peerStatus', { status });
  }

  return t('transfer:tasks.events.untranslated');
}

export function formatTaskEvent(value: string, language: string, t: Translate, fallback?: string): string {
  const match = value.match(/^(\d{4}-\d{2}-\d{2}T\S+?)(\s+)(.*)$/);
  const stamp = match?.[1] ?? fallback;
  const text = translateTaskEvent(value, t);
  if (!stamp) return text;
  const date = new Date(stamp);
  if (Number.isNaN(date.getTime())) return text;
  return `${date.toLocaleString(language, {
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
  }).replace(/-/g, '/')} ${text}`;
}
