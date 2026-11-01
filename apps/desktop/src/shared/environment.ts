/** The installed identity. The host's half (data folder, port, service names) mirrors serve/host/src/environment.rs. */
export const identity = {
  name: 'Kith',
  appId: 'com.yingkailiao.kith',
  scheme: 'kith',
  dataFolder: '.kith/serve',
  port: 19224,
  hostLabel: 'dev.kith.serve',
  systemdUnit: 'kith-serve.service',
  windowsService: 'KithServe',
  supervisor: 'kith-servew.exe',
  screenLabel: 'com.yingkailiao.kith.screen',
  screenName: 'KithScreen',
}
