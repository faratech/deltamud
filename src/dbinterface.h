/* MySQL includes */
#include <mysql.h>

extern MYSQL *SQLdb;
extern const char        *mySQL_host;
extern const unsigned int mySQL_port;
extern const char        *mySQL_user;
extern const char        *mySQL_pass;

int delete_player_entry (int idnum);
int insert_player_entry (struct char_data *ch);
struct char_data *retrieve_player_entry (char *name, struct char_data *ch);
int count_player_entries ( void );
void QUERY_DATABASE(MYSQL *db, char *query, int len);
MYSQL_RES *STORE_RESULT (MYSQL *db);
MYSQL_ROW FETCH_ROW (MYSQL_RES *result);
void pe_printf (char *name, char *types, char *querystr, ...);

#define ATOIROW(i) (!row[i] ? 0 : atoi(row[i]))

/* MySQL compatibility functions are already defined in mysql.h */