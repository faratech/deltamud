#if !defined(_BUFFER_H_)
#define _BUFFER_H_

/* Do NOT define this. */
/* #define BUFFER_TEST	1 */

/*
 * #if 1 = use buffer system for memory allocations. (not done, don't use)
 * #if 0 = use standard calloc/realloc
 */
#if 0
#define BUFFER_MEMORY	1
#endif

/*
 * #if 1 = use a threaded buffer system. (You must have pthreads.)
 * #if 0 = use the standard heartbeat() method.
 */
#if 1
#define THREADED	1
#endif

/*
 * #if 1 = return a pointer to the buffer structures
 * #if 0 = return only the data buffer (define this or all hell will break)
 */
#if 0
#define BUFFER_SNPRINTF	1
typedef struct buf_data buffer;
#else
typedef char buffer;
#endif

/*
 * #if 1 = Include original CircleMUD buffers too.
 * #if 0 = Use only new buffer system.
 */
#if 1
#define USE_CIRCLE_BUFFERS 1
#endif

/*
 * #if 1 = maintain a variable of how large a buffer the function requested.
 *      This will keep better track of buffer overruns when we give them a
 *      bigger buffer than requested.  An overrun with this on does not
 *      necessarily mean anything has been corrupted, but it does mean that
 *      it will happen and you have a serious problem. (recommended)
 * #if 0 = save sizeof(int) bytes per buffer structure and only check for
 *      overruns that corrupt memory past the buffer and do bad things.
 *
 * I'm thinking of removing this in favor of always being picky.
 */
#if 1
#define PICKY_BUFFERS   1
#else
#define req_size	size
#endif

/* *** No tweakables below *** */

/*
 * Handle GCC-isms.
 */
#if !defined(__GNUC__)
#define __attribute__(x)
#define __FUNCTION__	__FILE__
#endif

/*
 * Some macros to imitate C++ class styles. release_buffer() automatically
 * NULL's a pointer to prevent further use.
 */
#define get_buffer(a)		acquire_buffer((a), BT_STACK, __FUNCTION__, __LINE__)
#define release_buffer(a)	do { detach_buffer((a), __FUNCTION__, __LINE__); (a) = NULL; } while(0)
#define release_my_buffers()	detach_my_buffers(__FUNCTION__, __LINE__)

/*
 * Types for the memory to allocate.
 */
#define BT_STACK	0	/* Stack type memory			*/
#define BT_PERSIST	1	/* A buffer that doesn't time out	*/
#define BT_MALLOC	2	/* A malloc() implementation		*/

#define PULSE_BUFFER	(5 RL_SEC)

/*
 * Assorted lock types.
 */
#define LOCK_NONE	0
#define LOCK_OPEN	0
#define LOCK_RO		1
#define LOCK_CLOSED	2
#define LOCK_ACQUIRE	9
#define LOCK_WILL_CLEAR	10
#define LOCK_WILL_FREE	11
#define LOCK_WILL_REMOVE	12

/*
 * Public functions for outside use.
 */
#if defined(BUFFER_SNPRINTF)
buffer *str_cpy(buffer *d, buffer*s);
int bprintf(buffer *buf, const char *format, ...);
#endif
#if defined(BUFFER_MEMORY)
void *debug_calloc(size_t number, size_t size, char *func, int line);
void *debug_realloc(void *ptr, size_t size, const char *func, int line);
void debug_free(void *ptr);
#endif
void init_buffers(void);
void exit_buffers(void);
void release_all_buffers(void);
int detach_buffer(buffer *data, const char *func, const int line_n);
void detach_my_buffers(const char *func, const int line_n);

/* had to change this, won't compile under linux -Mike- 6/15/00 */
/* buffer *acquire_buffer(size_t size, int type, const char *who, ulong line); */

buffer *acquire_buffer(size_t size, int type, const char *who, int line);

void show_buffers(struct char_data *ch);

extern int buffer_cache_hits;
extern int buffer_cache_misses;

#if defined(BUFFER_SNPRINTF) || defined(_BUFFER_C_)
struct buf_data {
  bool locked;		/* Don't touch this buffer.		*/
  bool used;            /* Is someone using this buffer?        */
  byte type;		/* What type of buffer are we?		*/
  size_t size;          /* How large is this buffer?            */
  ulong line;         /* What source code line is using this. */
  long life;          /* An idle counter to free unused ones. */
#if defined(PICKY_BUFFERS)
  long req_size;      /* How much did the function request?   */
#endif
  char *data;           /* The buffer passed back to functions. */
  const char *who;      /* Name of the function using this.     */
  struct buf_data *next;        /* The next structure.          */
};
#endif

#endif
