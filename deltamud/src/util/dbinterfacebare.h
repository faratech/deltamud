#include "mysql.h"

#ifdef DBINTERFACEBARE_C
MYSQL *SQLdb;
#else
extern MYSQL *SQLdb;
#endif

#ifdef DBINTERFACEBARE_C
  const char        *mySQL_host="127.0.0.1";
  const unsigned int mySQL_port=3306;
  const char        *mySQL_user="root";
  const char        *mySQL_pass="uidxm4p5";
#else
  extern const char *mySQL_host, *mySQL_user, *mySQL_pass;
  extern const unsigned int mySQL_port;
#endif

void QUERY_DATABASE(MYSQL *db, char *query, int len);
MYSQL_RES *STORE_RESULT (MYSQL *db);
MYSQL_ROW FETCH_ROW (MYSQL_RES *result);

#define ATOIROW(i) (!row[i] ? 0 : atoi(row[i]))
