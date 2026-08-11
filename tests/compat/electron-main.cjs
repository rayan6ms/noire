"use strict";

const { app, BrowserWindow, session } = require("electron");

app.commandLine.appendSwitch("autoplay-policy", "no-user-gesture-required");
app.commandLine.appendSwitch("disable-gpu");

app.whenReady().then(async () => {
  session.defaultSession.setPermissionCheckHandler((_webContents, permission) =>
    permission === "media",
  );
  session.defaultSession.setPermissionRequestHandler(
    (_webContents, permission, callback) => callback(permission === "media"),
  );
  const window = new BrowserWindow({
    show: false,
    webPreferences: { backgroundThrottling: false },
  });
  const url = process.argv.find((argument) => argument.startsWith("http://"));
  if (!url) {
    throw new Error(`WebRTC fixture URL absent from ${JSON.stringify(process.argv)}`);
  }
  await window.loadURL(url);
  setTimeout(() => app.exit(2), 15000);
}).catch((error) => {
  console.error(error);
  app.exit(3);
});
