#include "mysql.h"
MYSQL *SQLdb;

#ifdef DBINTERFACEBARE_C
  const char        *mySQL_host="localhost";
  const unsigned int mySQL_port=4001;
  const char        *mySQL_user="system-mud";
  const char        *mySQL_pass="v5f9J8z0lm883jdks83jf45kj32l5hlh5k3j25k2jlj23h23";
#else
  extern const char *mySQL_host, *mySQL_user, *mySQL_pass;
  extern const unsigned int mySQL_port;
#endif

void QUERY_DATABASE(MYSQL *db, char *query, int len);
MYSQL_RES *STORE_RESULT (MYSQL *db);
MYSQL_ROW FETCH_ROW (MYSQL_RES *result);

#define ATOIROW(i) (!row[i] ? NULL : atoi(row[i]))
