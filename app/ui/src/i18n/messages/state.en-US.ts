import type { zhState } from './state.zh-CN';

/** state 组英文文案：键与中文切片一致 */
export const enState: Record<keyof typeof zhState, string> = {
  // —— Server/profile action notifications (app-store) ——
  'state.localNoSftp': 'Local sessions do not support SFTP',
  'state.localNoMetrics': 'Local sessions do not support server monitoring',
  'state.copyName': '{name} Copy',
  'state.copyNameN': '{name} Copy {n}',
  'state.sessionDuplicated': 'Duplicated as "{name}" (credentials are not copied)',
  'state.sessionDuplicatedWithTunnels':
    'Duplicated as "{name}" ({count} port forwards included, not started; credentials are not copied)',
  'state.duplicateSessionFailed': 'Failed to duplicate server: {error}',
  'state.favorited': 'Added to Favorites',
  'state.unfavorited': 'Removed from Favorites',
  'state.sessionDeleted': 'Server "{name}" deleted',
  'state.deleteSessionFailed': 'Failed to delete server: {error}',
  'state.exported': 'Exported: {path}',
  'state.openInExplorer': 'Reveal in File Explorer',
  'state.exportFailed': 'Export failed: {error}',
  'state.importDone':
    'Import complete: {sessions} sessions / {tunnels} tunnels / {credentials} credentials',
  'state.importFailed': 'Import failed: {error}',

  // —— Notification action registry (notice-actions) ——
  'state.openDirFailed': 'Failed to open folder: {error}',
  'state.unknownNoticeAction': 'Unknown notification action: {id}',

  // —— Tunnels (tunnel-utils) ——
  'state.tunnelDuplicated': 'Tunnel duplicated (not started)',
  'state.startMode.withSession': 'With Server Connection',
  'state.startMode.autostart': 'On App Startup',
  'state.startMode.manual': 'Manual Start',
  'state.errBindHostRequired': 'Bind address is required',
  'state.errBindPortRange': 'Bind port must be between 1 and 65535',
  'state.errTargetRequired': 'Local/remote tunnels require a target address',
  'state.errTargetPortRange': 'Target port must be between 1 and 65535',
  'state.kindLocal': 'Local',
  'state.kindRemote': 'Remote',
  'state.tunnelsAllStarted': '{name} connected, {ok}/{total} linked tunnels started',
  'state.tunnelStartFailed': '{name} connected, but tunnel "{tunnel}" failed to start',
  'state.tunnelStartFailedErr': '{name} connected, but tunnel "{tunnel}" failed to start: {error}',
  'state.tunnelStartFailedMore':
    '{name} connected, but tunnel "{tunnel}" failed to start ({count} more failed)',
  'state.tunnelStartFailedErrMore':
    '{name} connected, but tunnel "{tunnel}" failed to start: {error} ({count} more failed)',

  // —— Transfer center (transfer-store) ——
  'state.uploadStarted': 'Started uploading {count} item(s)',
  'state.downloadStarted': 'Started downloading {count} item(s)',
  'state.uploadDone': '{count} upload(s) completed',
  'state.downloadDone': '{count} download(s) completed',
  'state.transferFailed': 'Transfer failed: {name}',
  'state.transferFailedWithError': 'Transfer failed: {name} ({error})',
  'state.transfersFailed': '{count} transfers failed (first: {name})',
  'state.subscribeFailed': 'Failed to subscribe to transfers: {error}',
  'state.historyCleared': 'Transfer history cleared',
  'state.clearHistoryFailed': 'Failed to clear history: {error}',
  'state.operationFailed': 'Operation failed: {error}',
  'state.originDeleted': 'The original server profile was deleted; cannot retry',
  'state.requeued': 'Re-queued (resumable)',
  'state.retryFailed': 'Retry failed: {error}',

  // —— Keybinding action names (keymap; shown in Settings/Command Palette) ——
  'state.key.copy': 'Copy Selection',
  'state.key.paste': 'Paste',
  'state.key.palette': 'Command Palette',
  'state.key.search': 'Find in Terminal',
  'state.key.newTab': 'New Session',
  'state.key.reopenClosedTab': 'Reopen Closed Tab',
  'state.key.closeTab': 'Close Tab',
  'state.key.nextTab': 'Next Tab',
  'state.key.prevTab': 'Previous Tab',
  'state.key.sftp': 'SFTP Panel',
  'state.key.metrics': 'Metrics Panel',
  'state.key.tunnels': 'Tunnels Panel',
  'state.key.settings': 'Settings',
  'state.key.splitRow': 'Split Right',
  'state.key.splitCol': 'Split Down',
  'state.key.zoomIn': 'Increase Font Size',
  'state.key.zoomOut': 'Decrease Font Size',
  'state.key.resetZoom': 'Reset Font Size',
  'state.key.nextPane': 'Next Pane',

  // —— Themes (themes) ——
  'state.themeCustom': 'Custom',

  // —— Terminal view (TerminalView) ——
  'state.clipboardReadFailed': 'Failed to read the clipboard; it may be in use by another program',
  'state.connectFailed': 'Connection failed: {error}',
  'state.reconnecting': '[Connection lost, reconnecting (attempt {attempt})…]',
  'state.reconnected': '[Reconnected]',
  'state.connClosed': '[Connection closed]',
  'state.reconnectExhausted': 'Connection lost; automatic reconnect gave up after {count} attempts',
  'state.reconnectingNow': '[Reconnecting…]',
  'state.sessionNotFound': 'Session profile not found; it may have been deleted',
  'state.reconnectNow': 'Reconnect Now',
  'state.editConnection': 'Edit Connection',
  'state.close': 'Close',
  'state.pasteConfirmTitle': 'Paste Multiple Lines?',
  'state.pasteConfirmBody': 'This will paste {lines} lines, which may contain multiple commands.',
  'state.pasteConfirmWarn': 'Make sure you trust the content before pasting.',
  'state.menu.copy': 'Copy',
  'state.menu.selectAll': 'Select All',
  'state.menu.search': 'Search',
  'state.menu.clearScreen': 'Clear Screen',
  'state.menu.clearScrollback': 'Clear Scrollback',
  'state.menu.reconnect': 'Reconnect',
  'state.menu.openSftp': 'Open SFTP',
};
