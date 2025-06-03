#define DEFAULT_AUCTIONEER	"Arius"

/* Make up our auction data type and the 'auction' variable */
struct auction_data {
  long seller;
  long bidder;
  struct obj_data *obj;
  long bid;
  int ticks;
  char *auctioneer;
};

/* Function declarations */
struct char_data *get_ch_by_id_desc(long idnum);
struct char_data *get_ch_by_id(long idnum);
void auction_output(char *color, char *black);
void auction_reset(void);
void auction_update(void);
ACMD(do_bid);
ACMD(do_auction);
ACMD(do_auctioneer);

/* #define's for 'ticks' variable */
#define AUC_NONE	-1
#define AUC_NEW		0
#define AUC_BID		1
#define AUC_ONCE	2
#define AUC_TWICE       3
#define AUC_SOLD        4
