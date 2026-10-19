#ifndef PPB_TABLE_H
#define PPB_TABLE_H
struct ppb_iface { const char *name; void **table; int n; };
extern const struct ppb_iface ppb_ifaces[];
#endif
