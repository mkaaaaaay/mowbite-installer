// mowbite-usb-driver: installs WinUSB for the Tron in USB mode (Rockchip 2207:350b) with libwdi.dll. Started elevated
// by mowbite-installer with the path of a file for one line on what it did.
// Exit codes: 0 installed or WinUSB already, 1 usage, 2 no such device, 3 preparing failed, 4 installing failed.
#include <stdio.h>
#include <string.h>
#include <windows.h>

#include "libwdi.h"

#define TRON_VID 0x2207
#define TRON_PID 0x350b
#define INF_NAME "mowbite-tron.inf"

static FILE* result;

static int finish(int code, const char* what, const char* detail) {
  if (result) {
    fprintf(result, "%s%s%s\n", what, detail ? ": " : "", detail ? detail : "");
    fclose(result);
  }
  return code;
}

int main(int argc, char** argv) {
  if (argc != 2) return 1;
  result = fopen(argv[1], "w");

  struct wdi_options_create_list list_options = {0};
  list_options.list_all = TRUE;
  list_options.trim_whitespaces = TRUE;
  struct wdi_device_info* list = NULL;
  int r = wdi_create_list(&list, &list_options);
  if (r != WDI_SUCCESS) return finish(2, "no USB device found", wdi_strerror(r));

  struct wdi_device_info* tron = NULL;
  for (struct wdi_device_info* d = list; d != NULL; d = d->next) {
    if (d->vid == TRON_VID && d->pid == TRON_PID && !d->is_composite) {
      tron = d;
      break;
    }
  }
  if (tron == NULL) {
    wdi_destroy_list(list);
    return finish(2, "the Tron is not on USB", NULL);
  }
  if (tron->driver != NULL && _stricmp(tron->driver, "WinUSB") == 0) {
    wdi_destroy_list(list);
    return finish(0, "WinUSB was installed already", NULL);
  }

  char dir[MAX_PATH];
  DWORD n = GetTempPathA(sizeof(dir), dir);
  if (n == 0 || n + sizeof("mowbite-usb-driver") >= sizeof(dir)) {
    wdi_destroy_list(list);
    return finish(3, "preparing the driver failed", "no folder for temporary files");
  }
  strcat(dir, "mowbite-usb-driver");

  struct wdi_options_prepare_driver prepare = {0};
  prepare.driver_type = WDI_WINUSB;
  prepare.vendor_name = "Rockchip";
  r = wdi_prepare_driver(tron, dir, INF_NAME, &prepare);
  if (r != WDI_SUCCESS) {
    wdi_destroy_list(list);
    return finish(3, "preparing the driver failed", wdi_strerror(r));
  }
  struct wdi_options_install_driver install = {0};
  r = wdi_install_driver(tron, dir, INF_NAME, &install);
  wdi_destroy_list(list);
  if (r != WDI_SUCCESS) return finish(4, "installing the driver failed", wdi_strerror(r));
  return finish(0, "WinUSB installed", NULL);
}
