# Generated automatically from Makefile.in by configure.
# CircleMUD 3.0 Makefile.in - Makefile template used by 'configure'
#

# C compiler to use
CC = gcc

# Any special flags you want to pass to the compiler
MYFLAGS = -Wall -I/usr/local/include

#flags for profiling (see hacker.doc for more information)
PROFILE = 

##############################################################################
# Do Not Modify Anything Below This Line (unless you know what you're doing) #
##############################################################################

BINDIR = ../bin

CFLAGS = -g -O2 $(MYFLAGS) $(PROFILE)

OBJFILES = comm.o act.comm.o act.create.o act.informative.o act.movement.o act.item.o \
	act.offensive.o act.other.o act.social.o act.wizard.o alias.o ban.o boards.o \
	castle.o class.o config.o constants.o db.o fight.o graph.o arena.o handler.o \
	house.o interpreter.o language.o limits.o magic.o mail.o mobact.o modify.o \
	objsave.o olc.o random.o shop.o spec_assign.o spec_procs.o \
	spell_parser.o spells.o utils.o weather.o redit.o oedit.o zedit.o \
	medit.o sedit.o auction.o color.o races.o quest.o clan.o deity.o \
        dg_comm.o dg_db_scripts.o dg_event.o dg_handler.o dg_mobcmd.o \
        dg_objcmd.o dg_scripts.o dg_triggers.o dg_wldcmd.o dg_olc.o \
        maputils.o hedit.o aedit.o dbinterface.o

CXREF_FILES = act.comm.c act.create.c act.informative.c act.item.c act.movement.c \
	act.offensive.c act.other.c act.social.c act.wizard.c alias.c ban.c boards.c \
	castle.c clan.c class.c comm.c config.c constants.c db.c fight.c graph.c arena.c \
	handler.c house.c interpreter.c language.c limits.c magic.c mail.c mobact.c \
	modify.c objsave.c olc.c random.c shop.c spec_assign.c spec_procs.c \
	spell_parser.c spells.c utils.c weather.c redit.c oedit.c zedit.c \
	medit.c sedit.c auction.c color.c races.c quest.c clan.c deity.c \
        dg_comm.c dg_db_scripts.c dg_event.c dg_handler.c dg_mobcmd.c \
        dg_objcmd.c dg_scripts.c dg_triggers.c dg_wldcmd.c dg_olc.c \
        maputils.c hedit.c aedit.c dbinterface.c

default: all

all: .accepted
	$(MAKE) $(BINDIR)/circle
	$(MAKE) utils

.accepted:
	@./licheck less

backup:
	cp *.c *.h ~/extcoders/src
	cd ~/extcoders; 'rm' delta.tgz; tar zcf delta.tgz src

utils: .accepted
	(cd util; $(MAKE) all)

circle:
	$(MAKE) $(BINDIR)/circle

$(BINDIR)/circle : $(OBJFILES)
	$(CC) -o $(BINDIR)/circle $(PROFILE) $(OBJFILES)   -L/usr/local/lib/ -L/usr/local/lib/mysql /scsi2/server1/seadog/deltamud/src/libdescrypt.a /scsi2/server1/seadog/deltamud/src/libmysqlclient.a -lm # Used to have -lcrypt -- took it out.
clean:
	rm -f *.o
ref:
#
# Create the cross reference files
# Note, this is not meant to be used unless you've installed cxref...
#
	@for file in $(CXREF_FILES) ; do \
	  echo Cross referencing $$file ; \
	  /home/jelson/cxref/cxref -D__CXREF__ -xref -Odoc -Ncircle $$file ; \
	done
#
# Create the source files using /home/jelson/cxref/cxref
#
	@for file in $(CXREF_FILES) ; do \
	   echo Documenting $$file ; \
	   ( cd . ; /home/jelson/cxref/cxref -D__CXREF__ -warn-xref -xref -Odoc -Ncircle -html $$file ) ; \
	   rm -f $(DOCS) ; \
	done
#
# Create the index using /home/jelson/cxref/cxref
#
	@echo Indexing
	@( cd . ; /home/jelson/cxref/cxref -D__CXREF__ -index-all -Odoc -Ncircle -html )
	@rm -f $(DOCS)
#
# Make html files for the .h files
#
	@echo Creating .h.html files...
	@for file in *.h ; do \
	  echo $$file ; \
	  cat /home/jelson/mud/htmlh-head $$file /home/jelson/mud/htmlh-tail > doc/$$file.html ; \
	done
# Copy over to the html directory
	cp doc/*.html /home/jelson/public_html/circle/cdp/cxref
	chmod 644 /home/jelson/public_html/circle/cdp/cxref/*.html

# Dependencies for the object files (automagically generated with

$(BINDIR)/plrtoascii: plrtoascii.c $(INCDIR)/conf.h $(INCDIR)/sysdep.h \
	$(INCDIR)/structs.h
	$(CC) $(CFLAGS) -o ../bin/plrtoascii util/plrtoascii.c
# gcc -MM)
