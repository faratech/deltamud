# Generated automatically from Makefile.in by configure.
# CircleMUD 3.0 Makefile.in - Makefile template used by 'configure'
# for the 'util' directory

# C compiler to use
CC = gcc

# Any special flags you want to pass to the compiler
MYFLAGS = -Wall -DCIRCLE_UTIL -I/usr/include/mysql

#flags for profiling (see hacker.doc for more information)
PROFILE = 

#special linker libraries
LIBLINK = -lm -lmysqlclient

##############################################################################
# Do Not Modify Anything Below This Line (unless you know what you're doing) #
##############################################################################

# binary destination directory
BINDIR = ../../bin
# location of Circle include files
INCDIR = ..

CFLAGS = -g -O2 $(MYFLAGS) $(PROFILE) -I$(INCDIR)

default: all

all: dbinterfacebare.o autowiz.o $(BINDIR)/autowiz $(BINDIR)/delobjs $(BINDIR)/listrent \
	$(BINDIR)/mudpasswd $(BINDIR)/play2to3 $(BINDIR)/purgeplay \
	$(BINDIR)/shopconv $(BINDIR)/showplay $(BINDIR)/sign $(BINDIR)/split \
	$(BINDIR)/wld2html

dbinterfacebare: dbinterfacebare.o

autowizobj: autowiz.o

autowiz: $(BINDIR)/autowiz

delobjs: $(BINDIR)/delobjs

listrent: $(BINDIR)/listrent

mudpasswd: $(BINDIR)/mudpasswd

play2to3: $(BINDIR)/play2to3

purgeplay: $(BINDIR)/purgeplay

shopconv: $(BINDIR)/shopconv

showplay: $(BINDIR)/showplay

sign: $(BINDIR)/sign

split: $(BINDIR)/split

wld2html: $(BINDIR)/wld2html

dbinterfacebare.o: dbinterfacebare.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h $(INCDIR)/utils.h $(INCDIR)/db.h $(INCDIR)/mysql.h
	$(CC) $(CFLAGS) -c dbinterfacebare.c

autowizobj: autowiz.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h $(INCDIR)/utils.h $(INCDIR)/db.h $(INCDIR)/mysql.h
	$(CC) $(CFLAGS) -c autowiz.c

$(BINDIR)/autowiz: dbinterfacebare.o autowiz.o
	$(CC) $(CFLAGS) -o $(BINDIR)/autowiz dbinterfacebare.o autowiz.o $(LIBLINK)

$(BINDIR)/delobjs: delobjs.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h $(INCDIR)/utils.h
	$(CC) $(CFLAGS) -o $(BINDIR)/delobjs delobjs.c

$(BINDIR)/listrent: listrent.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h
	$(CC) $(CFLAGS) -o $(BINDIR)/listrent listrent.c

$(BINDIR)/mudpasswd: mudpasswd.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h $(INCDIR)/utils.h
	$(CC) $(CFLAGS) -o $(BINDIR)/mudpasswd mudpasswd.c -lcrypt

$(BINDIR)/play2to3: play2to3.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h
	$(CC) $(CFLAGS) -o $(BINDIR)/play2to3 play2to3.c

$(BINDIR)/purgeplay: purgeplay.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h $(INCDIR)/utils.h
	$(CC) $(CFLAGS) -o $(BINDIR)/purgeplay purgeplay.c

$(BINDIR)/shopconv: shopconv.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h $(INCDIR)/db.h $(INCDIR)/utils.h $(INCDIR)/shop.h
	$(CC) $(CFLAGS) -o $(BINDIR)/shopconv shopconv.c

$(BINDIR)/showplay: showplay.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h
	$(CC) $(CFLAGS) -o $(BINDIR)/showplay showplay.c

$(BINDIR)/sign: sign.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h
	$(CC) $(CFLAGS) -o $(BINDIR)/sign sign.c 

$(BINDIR)/split: split.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h
	$(CC) $(CFLAGS) -o $(BINDIR)/split split.c

$(BINDIR)/wld2html: wld2html.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h
	$(CC) $(CFLAGS) -o $(BINDIR)/wld2html wld2html.c
