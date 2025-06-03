#include "conf.h"
#include "sysdep.h"
#include "mysql.h"

#include "structs.h"
#include "utils.h"
#include "db.h"

#define DBINTERFACEBARE_C

#include "dbinterfacebare.h"

/* MySQL Database Connection Routines */
void connect_database (void) {
  SQLdb = (MYSQL *) malloc (sizeof(MYSQL));

  mysql_init(SQLdb);

  if (!mysql_real_connect(SQLdb, mySQL_host, mySQL_user, mySQL_pass, "players", mySQL_port, NULL, 0)) exit(0);
}

void QUERY_DATABASE(MYSQL *db, char *query, int len) {
  static char tries=0;
  if (mysql_real_query(db, query, len)) { /* This is BAD! */
    if (tries>=2)
      exit(0);
    tries++;
    mysql_close(SQLdb);			      /* Whether the DB connection died or something else... reset the connection and try again. */
    connect_database();
    QUERY_DATABASE(db, query, len); /* Retry. */
  }
  tries=0;
}

MYSQL_RES *STORE_RESULT (MYSQL *db) {
  MYSQL_RES *result;
  if (!(result=mysql_store_result(db))) return NULL;
  return result;
}

MYSQL_ROW FETCH_ROW (MYSQL_RES *result) {
  MYSQL_ROW row;
  if (!(row=mysql_fetch_row(result)) && mysql_errno(SQLdb))
    return NULL;
  return row;
}
